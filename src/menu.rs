//! What you see when you double-click the app: first-time setup, then a
//! small menu. Syncing itself always happens in the background process.
//!
//! Every prompt says what pressing Enter does, and there are no hidden
//! defaults: a choice that creates or shares something must be typed.

use crate::config::{Access, Config, DeviceId, FolderConfig, Home, Invite, PeerConfig};
use crate::service;
use crate::ui;
use crate::update::{self, Release};
use anyhow::Result;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

/// Free-text question; Enter uses `default` (shown in the prompt).
fn ask(question: &str, default: &str) -> String {
    ui::prompt(&format!("{question} {}: ", ui::dim(&format!("(Enter = {default})"))));
    let line = read_line();
    if line.is_empty() {
        default.to_string()
    } else {
        line
    }
}

/// Yes/no; anything other than "y"/"yes" (including Enter) means no.
fn confirm(question: &str) -> bool {
    ui::prompt(&format!("{question} {}: ", ui::dim("(y = yes, Enter = no)")));
    matches!(read_line().to_lowercase().as_str(), "y" | "yes")
}

/// Numbered choice with no default: the user must type a number.
/// Enter or 0 returns None (the caller says what that means in `none`).
fn choose(options: &[(&str, &str)], none: &str) -> Option<usize> {
    for (i, (text, help)) in options.iter().enumerate() {
        println!("   {}  {:<14} {}", ui::cyan(&format!("{:>2}", i + 1)), text, ui::dim(help));
    }
    println!();
    loop {
        ui::prompt(&format!("Type a number {}: ", ui::dim(&format!("(Enter = {none})"))));
        let line = read_line();
        if line.is_empty() || line == "0" {
            return None;
        }
        match line.parse::<usize>() {
            Ok(n) if n >= 1 && n <= options.len() => return Some(n - 1),
            _ => ui::warn(&format!("Type a number from 1 to {}.", options.len())),
        }
    }
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

/// Shows a numbered list and returns the chosen index (None = back).
fn pick(what: &str, items: &[String], none: &str) -> Option<usize> {
    for (i, item) in items.iter().enumerate() {
        println!("   {}  {item}", ui::cyan(&format!("{:>2}", i + 1)));
    }
    println!();
    loop {
        ui::prompt(&format!("Which {what}? {}: ", ui::dim(&format!("(Enter = {none})"))));
        let answer = read_line();
        if answer.is_empty() || answer == "0" {
            return None;
        }
        match answer.parse::<usize>() {
            Ok(n) if n >= 1 && n <= items.len() => return Some(n - 1),
            _ => ui::warn(&format!("Type a number from 1 to {}.", items.len())),
        }
    }
}

/// Asks which access a device gets. Some(None) = no access, None = cancelled.
fn ask_access(device: &str, none: &str) -> Option<Option<Access>> {
    ui::info(&format!("What may {device} do with this folder?"));
    let options = [
        ("Read & write", "gets the files and can change them"),
        ("Read only", "gets the files; changes made there are undone"),
        ("No access", "doesn't get this folder"),
    ];
    match choose(&options, none)? {
        0 => Some(Some(Access::ReadWrite)),
        1 => Some(Some(Access::ReadOnly)),
        _ => Some(None),
    }
}

fn access_text(access: Option<Access>) -> String {
    match access {
        Some(Access::ReadWrite) => ui::green("read & write"),
        Some(Access::ReadOnly) => ui::yellow("read only"),
        None => ui::dim("no access"),
    }
}

/// One line describing who a folder is shared with.
fn folder_role(cfg: &Config, f: &FolderConfig) -> String {
    if let Some(owner) = &f.owner {
        return format!("from {} - you have {}", cfg.peer_name(owner), access_text(Some(f.access)));
    }
    if f.shared_with.is_empty() {
        return format!("yours - {}", ui::yellow("not shared yet (choose 5)"));
    }
    let list: Vec<String> =
        f.shared_with.iter().map(|s| format!("{} {}", cfg.peer_name(&s.device), access_text(Some(s.access)))).collect();
    format!("yours - {}", list.join(", "))
}

// ---------------- first-time setup ----------------

fn setup(home: &Home) -> Result<()> {
    ui::clear();
    title_box("Welcome! Setup takes 4 short steps.");
    ui::info("Press Enter to accept a suggestion or skip a step - you can change everything later.");

    ui::section("Step 1 of 4 - name this device");
    ui::info("Your other devices will see this name.");
    let name = ask("Name", &crate::default_device_name());
    let key = home.load_or_create_key()?;
    let mut cfg = Config::new(name, 22100);
    home.save_config(&cfg)?;

    ui::section("Step 2 of 4 - this device's ID");
    ui::info("Each device has an ID. Devices only sync with IDs you add, so give this one");
    ui::info("to your other device (send it to yourself by email or chat).");
    show_id(&key.public);

    ui::section("Step 3 of 4 - connect your other device");
    ui::info("Its ID is shown in Mini Sync on that device (during its setup, or menu -> 1).");
    let peer = ask_peer(&key.public, "skip");
    match &peer {
        Some(p) => {
            cfg.upsert_peer(p.clone());
            home.save_config(&cfg)?;
            ui::ok(&format!("{} added.", p.name));
        }
        None => ui::info("Skipped. Add it later: menu -> 2."),
    }

    ui::section("Step 4 of 4 - share a folder from this device?");
    ui::info("Only do this on the device that HAS the files. On the other device, skip this");
    ui::info("step: it gets an invitation to join instead.");
    let options = [("Yes", "pick a folder on this device to share"), ("No", "skip (join or add a folder later)")];
    let mut shared = None;
    if choose(&options, "skip") == Some(0) {
        let default_folder = dirs::home_dir().unwrap_or_default().join("MiniSync");
        ui::info("Type or drag in a folder, or press Enter for the suggested one (created if needed).");
        let path = PathBuf::from(ask("Folder", &default_folder.display().to_string()));
        let mut folder = FolderConfig::new_own(path);
        if let Some(p) = &peer {
            println!();
            if let Some(access) = ask_access(&p.name, "decide later") {
                folder.set_share(&DeviceId::parse(&p.id)?, access);
                shared = access.map(|a| (p.name.clone(), a));
            }
        }
        ui::ok(&format!("Added {}.", folder.path.display()));
        cfg.folders.push(folder);
        home.save_config(&cfg)?;
    } else {
        ui::info("Skipped. No folder was created.");
    }

    println!();
    ui::info("Starting Mini Sync in the background...");
    match service::install(home, cfg.listen_port) {
        Ok(_) => ui::ok("Mini Sync is running and starts automatically when you log in."),
        Err(e) => ui::err(&format!("Could not install: {e:#}")),
    }

    ui::section("What's next");
    let mut step = 1;
    let mut next = |text: String| {
        println!("   {}  {text}", ui::cyan(&format!("{step}.")));
        step += 1;
    };
    match &peer {
        None => next("On your other device: open Mini Sync and add this device's ID (menu -> 2).".into()),
        Some(p) => next(format!("On {}: add this device's ID too, if you haven't yet (menu -> 2).", p.name)),
    }
    match (&shared, cfg.folders.is_empty()) {
        (Some((name, a)), _) => next(format!("On {name}: accept the folder invitation (menu -> 6). Access: {}.", a.label())),
        (None, false) => next("Choose who gets your folder: menu -> 5.".into()),
        (None, true) => next("To get a folder from another device: share it there, then accept it here (menu -> 6).".into()),
    }
    next("Open this menu any time: Mini Sync icon (Windows: by the clock; Mac: menu bar) -> Open Mini Sync.".into());
    pause();
    Ok(())
}

fn show_id(id: &DeviceId) {
    println!("\n      {}\n", ui::bold(&id.to_string()));
    if service::copy_to_clipboard(&id.to_string()) {
        ui::ok("Copied to the clipboard");
    }
}

/// `none` says what Enter does here ("skip" in setup, "back" in the menu).
fn ask_peer(me: &DeviceId, none: &str) -> Option<PeerConfig> {
    loop {
        ui::prompt(&format!("Paste the other device's ID {}: ", ui::dim(&format!("(Enter = {none})"))));
        let id = read_line();
        if id.is_empty() || id == "0" {
            return None;
        }
        match DeviceId::parse(&id) {
            Ok(parsed) if parsed == *me => ui::warn("That is this device's own ID. Paste the OTHER device's ID."),
            Ok(parsed) => {
                let name = ask("Name for that device (e.g. laptop, macbook)", "my-other-device");
                return Some(PeerConfig { name, id: parsed.to_string(), addresses: vec![] });
            }
            Err(_) => ui::warn("That doesn't look like a device ID (it looks like XXXXXXX-XXXXXXX-...). Try again."),
        }
    }
}

// ---------------- main menu ----------------

fn draw(cfg: &Config, key: &DeviceId, running: bool, upd: &UpdateInfo) {
    ui::clear();
    let (status_plain, status) = if running {
        ("● Running in the background".to_string(), ui::green("● Running in the background"))
    } else {
        ("● Not running - choose 10 to start".to_string(), ui::red("● Not running - choose 10 to start"))
    };
    let title_plain = format!("Mini Sync  v{}", update::CURRENT_VERSION);
    let title = format!("{}  {}", ui::accent("Mini Sync"), ui::dim(&format!("v{}", update::CURRENT_VERSION)));
    let mut lines = vec![(title_plain, title), (status_plain, status)];
    match cfg.pending_invites() {
        0 => {}
        n => {
            let t = format!("✉ {n} folder invitation{} - choose 6", if n == 1 { "" } else { "s" });
            lines.push((t.clone(), ui::yellow(&t)));
        }
    }
    match upd {
        UpdateInfo::Available(r) => {
            let t = format!("↑ Update v{} available - choose 14", r.version);
            lines.push((t.clone(), ui::yellow(&t)));
        }
        UpdateInfo::Checking => lines.push(("Checking for updates...".into(), ui::dim("Checking for updates..."))),
        _ => {}
    }
    ui::boxed(&lines);

    println!();
    ui::field("This device", &ui::bold(&cfg.device_name));
    ui::field("Device ID", &key.to_string());
    if cfg.peers.is_empty() {
        ui::field("Devices", &ui::yellow("none yet - choose 2 to add one"));
    } else {
        let names: Vec<&str> = cfg.peers.iter().map(|p| p.name.as_str()).collect();
        ui::field("Devices", &names.join(", "));
    }
    if cfg.folders.is_empty() {
        ui::field("Folders", &ui::yellow("none yet - choose 4 to add one, or 6 to join one"));
    }
    for (i, f) in cfg.folders.iter().enumerate() {
        let label = format!("{}  {}", ui::bold(&f.label()), ui::dim(&f.path.display().to_string()));
        ui::field(if i == 0 { "Folders" } else { "" }, &label);
        ui::field("", &folder_role(cfg, f));
    }

    let invites = match cfg.pending_invites() {
        0 => "Invitations".to_string(),
        n => format!("Invitations ({n} new)"),
    };
    let item = |n: &str, text: &str| {
        if n.is_empty() {
            " ".repeat(30)
        } else {
            format!("{}  {:<26}", ui::cyan(&format!("{n:>2}")), text)
        }
    };
    let row = |a: (&str, &str), b: (&str, &str)| println!("   {}  {}", item(a.0, a.1), item(b.0, b.1));

    ui::section("Devices                          Folders");
    row(("1", "Copy this device's ID"), ("4", "Add a folder"));
    row(("2", "Add or update a device"), ("5", "Share a folder / access"));
    row(("3", "Remove a device"), ("6", &invites));
    row(("", ""), ("7", "Remove a folder"));
    row(("", ""), ("8", "Open a synced folder"));

    ui::section("App                              Security & updates");
    row(("9", "View log"), ("12", "Change this device's ID"));
    row(("10", if running { "Restart" } else { "Start" }), ("13", "Uninstall"));
    row(("11", "Stop until next login"), ("14", "Check for updates"));

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
            "2" => add_device(home, &cfg, &key.public)?,
            "3" => remove_device(home, &cfg)?,
            "4" => add_folder(home, &cfg)?,
            "5" => share_menu(home, &cfg)?,
            "6" => invitations(home, &cfg)?,
            "7" => remove_folder(home, &cfg)?,
            "8" => match cfg.folders.len() {
                0 => {
                    ui::info("There are no folders yet.");
                    pause();
                }
                1 => service::open_path(&cfg.folders[0].path),
                _ => {
                    ui::section("Open a synced folder");
                    let labels: Vec<String> = cfg.folders.iter().map(|f| f.label()).collect();
                    if let Some(i) = pick("folder", &labels, "back") {
                        service::open_path(&cfg.folders[i].path);
                    }
                }
            },
            "9" => service::open_path(&service::log_path(home)),
            "10" => {
                restart(home, &cfg);
                pause();
            }
            "11" => {
                if confirm("Stop syncing until next login?") {
                    service::stop(cfg.listen_port);
                    ui::ok("Stopped. It starts again at next login (or choose 10).");
                    pause();
                }
            }
            "12" => {
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
            "13" => {
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
            "14" => {
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

// ---------------- devices ----------------

fn add_device(home: &Home, cfg: &Config, me: &DeviceId) -> Result<()> {
    ui::section("Add or update a device");
    if !cfg.peers.is_empty() {
        ui::info("To update a device that got a new ID, use the same name as before.");
    }
    let Some(peer) = ask_peer(me, "back") else { return Ok(()) };
    let mut cfg = cfg.clone();
    let name = peer.name.clone();
    let id = peer.id.clone();
    let replaced = cfg.upsert_peer(peer);
    home.save_config(&cfg)?;
    if replaced {
        ui::ok(&format!("Updated {name}'s ID - the old ID is no longer trusted. Its folder access is kept."));
    } else {
        ui::ok(&format!("Added {name}."));
        ui::info(&format!("Also add this device's ID on {name} (there: menu -> 2), or they can't connect."));
        let mine: Vec<FolderConfig> = cfg.folders.iter().filter(|f| f.is_mine()).cloned().collect();
        if !mine.is_empty() {
            println!();
            ui::info(&format!("{name} gets none of your folders until you share one."));
            if confirm(&format!("Share a folder with {name} now?")) {
                let folder = if mine.len() == 1 {
                    Some(mine[0].clone())
                } else {
                    let labels: Vec<String> = mine.iter().map(|f| f.label()).collect();
                    pick("folder", &labels, "skip").map(|i| mine[i].clone())
                };
                if let Some(folder) = folder {
                    println!();
                    if let Some(Some(access)) = ask_access(&name, "skip") {
                        if let Some(f) = cfg.folders.iter_mut().find(|f| f.id == folder.id) {
                            f.set_share(&DeviceId::parse(&id)?, Some(access));
                        }
                        home.save_config(&cfg)?;
                        ui::ok(&format!("{name} gets {:?} ({}). It will see an invitation to accept.", folder.label(), access.label()));
                    }
                }
            }
        }
    }
    restart(home, &cfg);
    pause();
    Ok(())
}

fn remove_device(home: &Home, cfg: &Config) -> Result<()> {
    ui::section("Remove a device");
    if cfg.peers.is_empty() {
        ui::info("There is no device to remove.");
        pause();
        return Ok(());
    }
    let names: Vec<String> = cfg.peers.iter().map(|p| p.name.clone()).collect();
    let Some(i) = pick("device", &names, "back") else { return Ok(()) };
    let peer = cfg.peers[i].clone();
    let joined: Vec<String> =
        cfg.folders.iter().filter(|f| f.owner.as_ref() == Some(&peer.id)).map(|f| f.label()).collect();
    ui::info(&format!("{} loses access to all your folders. Nothing is deleted on either device.", peer.name));
    if !joined.is_empty() {
        ui::info(&format!("Folders you joined from {} stop syncing: {}", peer.name, joined.join(", ")));
    }
    if confirm(&format!("Remove {}?", peer.name)) {
        let mut cfg = cfg.clone();
        home.remove_peer(&mut cfg, &peer.name)?;
        ui::ok(&format!("Removed {}.", peer.name));
        restart(home, &cfg);
    }
    pause();
    Ok(())
}

// ---------------- folders ----------------

fn add_folder(home: &Home, cfg: &Config) -> Result<()> {
    ui::section("Add a folder to share");
    ui::info("Pick a folder on THIS device. You then choose which devices get it.");
    ui::info("(To get a folder another device shares with you, use 6 Invitations instead.)");
    ui::info("Tip: drag the folder into this window to paste its path.");
    println!();
    ui::prompt(&format!("Folder path {}: ", ui::dim("(Enter = back)")));
    let path = read_line();
    if path.is_empty() || path == "0" {
        return Ok(());
    }
    let path = PathBuf::from(path);
    if let Some(f) = cfg.folders.iter().find(|f| f.path == path) {
        ui::err(&format!("That folder is already synced ({}).", f.label()));
        pause();
        return Ok(());
    }
    let mut cfg = cfg.clone();
    let folder = FolderConfig::new_own(path);
    let id = folder.id.clone();
    cfg.folders.push(folder);
    home.save_config(&cfg)?;
    ui::ok(&format!("Added {}.", cfg.folders.last().map(|f| f.label()).unwrap_or_default()));
    if cfg.peers.is_empty() {
        ui::info("Add a device (2), then choose who gets this folder (5).");
    } else {
        ui::section("Who gets this folder?");
        share_folder(home, &mut cfg, &id)?;
    }
    restart(home, &cfg);
    pause();
    Ok(())
}

fn share_menu(home: &Home, cfg: &Config) -> Result<()> {
    ui::section("Share a folder / change access");
    let mine: Vec<&FolderConfig> = cfg.folders.iter().filter(|f| f.is_mine()).collect();
    if mine.is_empty() {
        ui::info("You don't own any folder yet (choose 4 to add one).");
        ui::info("Folders you joined are controlled by the device that shared them.");
        pause();
        return Ok(());
    }
    if cfg.peers.is_empty() {
        ui::info("Add a device first (choose 2).");
        pause();
        return Ok(());
    }
    let id = if mine.len() == 1 {
        mine[0].id.clone()
    } else {
        let labels: Vec<String> =
            mine.iter().map(|f| format!("{}  {}", f.label(), ui::dim(&f.path.display().to_string()))).collect();
        let Some(i) = pick("folder", &labels, "back") else { return Ok(()) };
        mine[i].id.clone()
    };
    let mut cfg = cfg.clone();
    if share_folder(home, &mut cfg, &id)? {
        restart(home, &cfg);
        pause();
    }
    Ok(())
}

/// Lets the user set each device's access to one folder. Returns true if anything changed.
fn share_folder(home: &Home, cfg: &mut Config, folder_id: &str) -> Result<bool> {
    let mut changed = false;
    loop {
        let Some(f) = cfg.folders.iter().find(|f| f.id == folder_id).cloned() else { return Ok(changed) };
        println!();
        ui::field("Folder", &format!("{}  {}", ui::bold(&f.label()), ui::dim(&f.path.display().to_string())));
        ui::info("Pick a device to set its access. Press Enter when you're done.");
        println!();
        let rows: Vec<String> = cfg
            .peers
            .iter()
            .map(|p| {
                let access = DeviceId::parse(&p.id).ok().and_then(|d| f.share_for(&d));
                format!("{:<20} {}", p.name, access_text(access))
            })
            .collect();
        let Some(i) = pick("device", &rows, "done") else { return Ok(changed) };
        let peer = cfg.peers[i].clone();
        let Some(access) = ask_access(&peer.name, "cancel") else { continue };
        let dev = DeviceId::parse(&peer.id)?;
        let before = f.share_for(&dev);
        if before == access {
            continue;
        }
        if let Some(f) = cfg.folders.iter_mut().find(|f| f.id == folder_id) {
            f.set_share(&dev, access);
        }
        home.save_config(cfg)?;
        changed = true;
        match (before, access) {
            (None, Some(a)) => ui::ok(&format!("{} gets {} access. It will see an invitation to accept (there: menu -> 6).", peer.name, a.label())),
            (Some(_), Some(a)) => ui::ok(&format!("{} now has {} access.", peer.name, a.label())),
            (_, None) => ui::ok(&format!("{} no longer gets this folder. Its copy is kept but stops syncing.", peer.name)),
        }
    }
}

fn invitations(home: &Home, cfg: &Config) -> Result<()> {
    ui::section("Invitations");
    if cfg.invites.is_empty() {
        ui::info("No invitations. When another device shares a folder with this one,");
        ui::info("it shows up here (both devices must be running and connected).");
        pause();
        return Ok(());
    }
    let rows: Vec<String> = cfg
        .invites
        .iter()
        .map(|i| {
            let tag = if i.declined { ui::dim(" (declined)") } else { String::new() };
            format!("{} from {} - {}{tag}", ui::bold(&i.label), cfg.peer_name(&i.from), access_text(Some(i.access)))
        })
        .collect();
    let n = if rows.len() == 1 {
        println!("   {}", rows[0]);
        0
    } else {
        let Some(n) = pick("invitation", &rows, "back") else { return Ok(()) };
        n
    };
    let inv = cfg.invites[n].clone();

    println!();
    ui::info(&format!("{} wants to share the folder {:?} with this device.", cfg.peer_name(&inv.from), inv.label));
    match inv.access {
        Access::ReadWrite => ui::info("Read & write: you get the files, and your changes are synced back."),
        Access::ReadOnly => {
            ui::info("Read only: you get the files, but changes you make there are undone");
            ui::info("(your edited copy is kept in .minisync-trash).");
        }
    }
    println!();
    let options = [("Accept", "choose where to keep it on this device"), ("Decline", "don't sync it")];
    match choose(&options, "back") {
        Some(0) => accept(home, cfg, &inv),
        Some(_) => {
            let mut cfg = cfg.clone();
            if let Some(i) = cfg.invites.iter_mut().find(|i| i.folder == inv.folder && i.from == inv.from) {
                i.declined = true;
            }
            home.save_config(&cfg)?;
            ui::ok("Declined. You can still accept it later from this list.");
            pause();
            Ok(())
        }
        None => Ok(()),
    }
}

fn accept(home: &Home, cfg: &Config, inv: &Invite) -> Result<()> {
    let default = dirs::home_dir().unwrap_or_default().join(&inv.label);
    println!();
    ui::info(&format!("Where should {:?} be kept on this device?", inv.label));
    ui::info("Type or drag in a folder, or press Enter for the suggested one (created if needed).");
    let path = PathBuf::from(ask("Folder", &default.display().to_string()));
    if !is_empty_dir(&path) {
        ui::warn(&format!("{} already has files in it.", path.display()));
        match inv.access {
            Access::ReadOnly => ui::warn("Files there that the owner doesn't have will be moved to .minisync-trash."),
            Access::ReadWrite => ui::warn("Files there will be merged and sent to the owner."),
        }
        if !confirm("Use it anyway?") {
            return Ok(());
        }
    }
    let mut cfg = cfg.clone();
    match crate::accept_invite(&mut cfg, inv, path) {
        Ok(()) => {
            home.save_config(&cfg)?;
            ui::ok(&format!("Joined {:?}.", inv.label));
            restart(home, &cfg);
        }
        Err(e) => ui::err(&format!("{e:#}")),
    }
    pause();
    Ok(())
}

fn is_empty_dir(path: &Path) -> bool {
    match std::fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => true, // doesn't exist yet
    }
}

fn remove_folder(home: &Home, cfg: &Config) -> Result<()> {
    ui::section("Remove a folder");
    if cfg.folders.is_empty() {
        ui::info("There is no folder to remove.");
        pause();
        return Ok(());
    }
    let labels: Vec<String> =
        cfg.folders.iter().map(|f| format!("{}  {}", f.label(), ui::dim(&f.path.display().to_string()))).collect();
    let Some(i) = pick("folder", &labels, "back") else { return Ok(()) };
    let folder = cfg.folders[i].clone();
    ui::info(&format!("This stops syncing {}. The files stay on this device.", folder.path.display()));
    if folder.is_mine() && !folder.shared_with.is_empty() {
        ui::info("Devices you shared it with stop getting updates (their copies are kept).");
    }
    if confirm("Remove it?") {
        let mut cfg = cfg.clone();
        home.remove_folder(&mut cfg, &folder.id)?;
        ui::ok("Removed.");
        restart(home, &cfg);
    }
    pause();
    Ok(())
}

/// Restarts the background sync so a change takes effect.
fn restart(home: &Home, cfg: &Config) {
    match service::restart(home, cfg.listen_port) {
        Ok(()) => ui::ok("Saved and applied."),
        Err(e) => ui::err(&format!("Saved, but Mini Sync could not restart: {e:#}")),
    }
}
