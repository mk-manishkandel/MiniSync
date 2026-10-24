// No console window on Windows; commands typed in a terminal re-attach to it.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod config;
mod folder;
mod menu;
mod net;
mod proto;
mod scan;
mod service;
#[cfg(any(windows, target_os = "macos"))]
mod tray;
mod ui;
mod update;
mod util;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use config::{Access, Config, DeviceId, FolderConfig, Home, PeerConfig};
use net::{FolderEvent, FolderHandle, Hub, Rule};
use notify::{RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(
    name = "minisync",
    version,
    about = "Sync folders between your own devices on the same network.\nRun without arguments for the setup/menu."
)]
struct Cli {
    /// Where config, key and index live (default: your OS config dir / minisync)
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create this device's key and an empty config
    Init {
        /// A friendly name for this device
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = 22100)]
        port: u16,
    },
    /// Print this device's ID (give it to your other devices)
    Id,
    /// Add a folder owned by this device (shared with nobody until you use `share`)
    AddFolder {
        path: PathBuf,
        /// Folder ID (default: generated from the folder's name)
        #[arg(long)]
        id: Option<String>,
    },
    /// Give a device access to a folder you own: read-write, read-only or none
    Share {
        /// Folder ID (see `show`)
        folder: String,
        /// Device name
        device: String,
        #[arg(value_parser = ["read-write", "read-only", "none"])]
        access: String,
    },
    /// Accept an invitation to a folder from another device
    Accept {
        /// Folder ID from the invitation (see `show`)
        folder: String,
        /// Where to keep the folder on this device
        path: PathBuf,
    },
    /// Decline an invitation
    Decline { folder: String },
    /// Trust another of your devices
    AddPeer {
        name: String,
        id: String,
        /// Optional fixed address like 192.168.1.20:22100
        #[arg(long)]
        address: Vec<String>,
    },
    /// Stop syncing a folder (files are kept)
    RemoveFolder { id: String },
    /// Stop trusting a device (by name or ID)
    RemovePeer { name: String },
    /// Show config
    Show,
    /// Start syncing in this window (runs until stopped)
    Run {
        /// Log to a file instead of the terminal (used by autostart)
        #[arg(long)]
        background: bool,
        /// Wait for the previous version to exit (used right after an update)
        #[arg(long, hide = true)]
        after_update: bool,
    },
    /// Install, start in the background and start at every login
    Install,
    /// Check GitHub for a newer version and install it
    Update,
    /// Stop, remove start-at-login, the program and its settings (synced files are kept)
    Uninstall {
        /// Keep settings and this device's ID
        #[arg(long)]
        keep_settings: bool,
    },
    /// Give this device a new ID (old ID stops working)
    NewId,
    /// Start the background sync
    Start,
    /// Stop the background sync
    Stop,
    /// Is the background sync running?
    Status,
}

fn main() {
    // Attach the console before parsing so `--help` and errors are visible.
    let args: Vec<_> = std::env::args_os().collect();
    let background = args.iter().any(|a| a == "--background");
    let interactive = args.len() == 1 || (args.len() == 3 && args[1] == "--home");
    if !background {
        console::attach(interactive);
    }
    let cli = Cli::parse_from(args);
    if let Err(e) = real_main(cli, background) {
        log::error!("{e:#}");
        eprintln!("error: {e:#}");
        if interactive {
            eprintln!("\nPress Enter to close.");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
        std::process::exit(1);
    }
}

fn real_main(cli: Cli, background: bool) -> Result<()> {
    let home = Home::new(cli.home)?;
    init_logging(&home, background);

    let Some(cmd) = cli.cmd else {
        return menu::run(&home);
    };
    match cmd {
        Cmd::Init { name, port } => {
            if home.config_path().exists() {
                bail!("{} already exists", home.config_path().display());
            }
            let key = home.load_or_create_key()?;
            let name = name.unwrap_or_else(default_device_name);
            home.save_config(&Config::new(name, port))?;
            println!("Created {}", home.config_path().display());
            println!("This device's ID:\n\n    {}\n", key.public);
        }
        Cmd::Id => println!("{}", home.load_or_create_key()?.public),
        Cmd::AddFolder { path, id } => {
            let mut cfg = home.load_config()?;
            let mut folder = FolderConfig::new_own(absolute(path)?);
            if let Some(id) = id {
                folder.id = id;
            }
            if cfg.folders.iter().any(|f| f.id == folder.id) {
                bail!("folder {:?} already exists", folder.id);
            }
            println!("Added {} as folder {:?}. Share it with `minisync share {} DEVICE read-write`.", folder.path.display(), folder.id, folder.id);
            cfg.folders.push(folder);
            home.save_config(&cfg)?;
        }
        Cmd::Share { folder, device, access } => {
            let mut cfg = home.load_config()?;
            let peer = cfg.peers.iter().find(|p| p.name == device).with_context(|| format!("no device named {device:?}"))?;
            let peer_id = DeviceId::parse(&peer.id)?;
            let f = cfg.folders.iter_mut().find(|f| f.id == folder).with_context(|| format!("no folder with ID {folder:?}"))?;
            if !f.is_mine() {
                bail!("only the device that owns {folder:?} can change who has access");
            }
            let access = match access.as_str() {
                "read-write" => Some(Access::ReadWrite),
                "read-only" => Some(Access::ReadOnly),
                _ => None,
            };
            f.set_share(&peer_id, access);
            home.save_config(&cfg)?;
            println!("{device}: {}. Run `minisync stop` then `minisync start` to apply.", access.map_or("no access", |a| a.label()));
        }
        Cmd::Accept { folder, path } => {
            let mut cfg = home.load_config()?;
            let inv = cfg.invites.iter().find(|i| i.folder == folder).cloned().with_context(|| format!("no invitation for {folder:?}"))?;
            let path = absolute(path)?;
            accept_invite(&mut cfg, &inv, path.clone())?;
            home.save_config(&cfg)?;
            println!("Joined {:?} at {} ({}). Run `minisync stop` then `minisync start` to apply.", inv.label, path.display(), inv.access.label());
        }
        Cmd::Decline { folder } => {
            let mut cfg = home.load_config()?;
            let inv = cfg.invites.iter_mut().find(|i| i.folder == folder).with_context(|| format!("no invitation for {folder:?}"))?;
            inv.declined = true;
            home.save_config(&cfg)?;
            println!("Declined.");
        }
        Cmd::AddPeer { name, id, address } => {
            let mut cfg = home.load_config()?;
            let parsed = DeviceId::parse(&id)?;
            if parsed == home.load_or_create_key()?.public {
                bail!("that is this device's own ID");
            }
            // Same name or same ID replaces the old entry (e.g. after the other device got a new ID).
            cfg.upsert_peer(PeerConfig { name: name.clone(), id: parsed.to_string(), addresses: address });
            home.save_config(&cfg)?;
            println!("Trusting {name}. It sees none of your folders until you share one with it.");
        }
        Cmd::RemoveFolder { id } => {
            let mut cfg = home.load_config()?;
            let f = home.remove_folder(&mut cfg, &id)?;
            println!("No longer syncing {} (files kept). Run `minisync stop` then `minisync start` to apply.", f.path.display());
        }
        Cmd::RemovePeer { name } => {
            let mut cfg = home.load_config()?;
            let p = home.remove_peer(&mut cfg, &name)?;
            println!("Removed {}. Run `minisync stop` then `minisync start` to apply.", p.name);
        }
        Cmd::Show => {
            println!("# {}", home.config_path().display());
            println!("# device id: {}\n", home.load_or_create_key()?.public);
            print!("{}", std::fs::read_to_string(home.config_path())?);
        }
        Cmd::Run { background, after_update } => {
            let cfg = home.load_config()?;
            // Holding the port doubles as a "only one copy running" lock.
            // Right after an update, give the old version a moment to exit.
            let deadline = std::time::Instant::now() + Duration::from_secs(if after_update { 20 } else { 0 });
            let listener = loop {
                match std::net::TcpListener::bind(("0.0.0.0", cfg.listen_port)) {
                    Ok(l) => break l,
                    Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                        if std::time::Instant::now() >= deadline {
                            bail!("Mini Sync is already running (port {} is in use)", cfg.listen_port)
                        }
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    Err(e) => return Err(e).with_context(|| format!("listening on port {}", cfg.listen_port)),
                }
            };
            if background {
                update::cleanup_old();
                log::info!("Mini Sync v{} starting", update::CURRENT_VERSION);
            }
            #[cfg(any(windows, target_os = "macos"))]
            if background {
                let updates: update::SharedUpdate = Arc::new(std::sync::Mutex::new(update::UpdateState::UpToDate));
                update::spawn_checker(updates.clone());
                // Sync runs on a worker thread; the tray icon needs the main thread.
                let status: tray::Status = Arc::new(std::sync::Mutex::new("starting".into()));
                let engine_status = status.clone();
                let engine_home = Home { dir: home.dir.clone() };
                std::thread::spawn(move || {
                    let result = tokio::runtime::Runtime::new()
                        .map_err(anyhow::Error::from)
                        .and_then(|rt| rt.block_on(run(engine_home, listener, Some(engine_status))));
                    if let Err(e) = &result {
                        log::error!("{e:#}");
                    }
                    std::process::exit(if result.is_ok() { 0 } else { 1 });
                });
                tray::run(home, status, updates);
            }
            let _ = background;
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(run(home, listener, None))?;
        }
        Cmd::Update => {
            println!("This is v{}. Checking GitHub...", update::CURRENT_VERSION);
            match update::check()? {
                None => println!("You have the latest version."),
                Some(rel) => {
                    println!("Installing v{}...", rel.version);
                    let cfg = home.load_config()?;
                    update::install_from_menu(&home, cfg.listen_port, &rel)?;
                    println!("Updated to v{} and running.", rel.version);
                }
            }
        }
        Cmd::NewId => {
            let cfg = home.load_config()?;
            let was_running = service::is_running(cfg.listen_port);
            service::stop(cfg.listen_port);
            let key = home.rotate_key()?;
            if was_running {
                service::start(&home, cfg.listen_port)?;
            }
            println!("New device ID:\n\n    {}\n", key.public);
            println!("On each of your other devices, add it again under the same name");
            println!("(minisync add-peer {} <NEW-ID>, or menu -> Add or update a device).", cfg.device_name);
        }
        Cmd::Install => {
            let cfg = home.load_config()?;
            let exe = service::install(&home, cfg.listen_port)?;
            println!("Installed to {} and running in the background.", exe.display());
        }
        Cmd::Uninstall { keep_settings } => {
            let port = home.load_config().map(|c| c.listen_port).unwrap_or(22100);
            service::uninstall(&home, port, keep_settings)?;
            if keep_settings {
                println!("Uninstalled. Settings were kept in {}; synced files were not touched.", home.dir.display());
            } else {
                println!("Uninstalled, including settings and this device's ID. Synced files were not touched.");
            }
        }
        Cmd::Start => {
            let cfg = home.load_config()?;
            service::start(&home, cfg.listen_port)?;
            println!("Running.");
        }
        Cmd::Stop => {
            let cfg = home.load_config()?;
            service::stop(cfg.listen_port);
            println!("Stopped.");
        }
        Cmd::Status => {
            let cfg = home.load_config()?;
            if service::is_running(cfg.listen_port) {
                println!("Running.");
            } else {
                println!("Not running.");
            }
        }
    }
    Ok(())
}

fn init_logging(home: &Home, background: bool) {
    let mut builder = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    builder.format_target(false);
    if background {
        let path = service::log_path(home);
        let _ = std::fs::create_dir_all(&home.dir);
        if std::fs::metadata(&path).map(|m| m.len() > 10 * 1024 * 1024).unwrap_or(false) {
            let _ = std::fs::rename(&path, path.with_extension("log.old"));
        }
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            builder.target(env_logger::Target::Pipe(Box::new(f)));
        }
    }
    builder.init();
}

async fn run(home: Home, listener: std::net::TcpListener, status: Option<Arc<std::sync::Mutex<String>>>) -> Result<()> {
    let cfg = home.load_config()?;
    let key = home.load_or_create_key()?;
    listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(listener)?;

    log::info!("this device: {} ({})", cfg.device_name, key.public);
    if cfg.folders.is_empty() {
        log::warn!("no folders configured; add one with `minisync add-folder`");
    }
    if cfg.peers.is_empty() {
        log::warn!("no peers configured; add one with `minisync add-peer`");
    }

    let peers: HashMap<DeviceId, (String, Vec<String>)> = cfg
        .peers
        .iter()
        .map(|p| Ok((DeviceId::parse(&p.id)?, (p.name.clone(), p.addresses.clone()))))
        .collect::<Result<_>>()?;

    let mut handles = HashMap::new();
    let mut receivers = Vec::new();
    for f in &cfg.folders {
        let (tx, rx) = mpsc::channel(1024);
        handles.insert(f.id.clone(), FolderHandle { root: f.path.clone(), events: tx.clone() });
        receivers.push((f.clone(), tx, rx));
    }

    let rules: HashMap<String, Rule> = cfg
        .folders
        .iter()
        .map(|f| {
            let shares = f
                .shared_with
                .iter()
                .filter_map(|s| DeviceId::parse(&s.device).ok().map(|d| (d, s.access)))
                .filter(|(d, _)| peers.contains_key(d))
                .collect();
            (f.id.clone(), Rule { owner: f.owner_id(), access: f.access, shares, label: f.label() })
        })
        .collect();
    for f in &cfg.folders {
        match f.owner_id() {
            Some(o) => log::info!("[{}] {} - from {}, {}", f.id, f.path.display(), cfg.peer_name(&o.to_string()), f.access.label()),
            None if f.shared_with.is_empty() => log::info!("[{}] {} - yours, not shared with any device yet", f.id, f.path.display()),
            None => {
                let list: Vec<String> =
                    f.shared_with.iter().map(|s| format!("{} ({})", cfg.peer_name(&s.device), s.access.label())).collect();
                log::info!("[{}] {} - yours, shared with {}", f.id, f.path.display(), list.join(", "));
            }
        }
    }

    let hub = Arc::new(Hub::new(
        key,
        cfg.device_name.clone(),
        cfg.listen_port,
        peers,
        handles,
        rules,
        home.dir.clone(),
        cfg.pending_invites(),
    ));

    let mut watchers = Vec::new();
    for (f, tx, rx) in receivers {
        if !f.path.exists() && !home.index_path(&f.id).exists() {
            std::fs::create_dir_all(&f.path).with_context(|| format!("creating {}", f.path.display()))?;
        }
        match watch(&f.path, tx) {
            Ok(w) => watchers.push(w),
            Err(e) => log::warn!("[{}] not watching for changes ({e:#}); will rescan every 5 minutes", f.id),
        }
        let folder = folder::Folder::new(f.id.clone(), f.path.clone(), f.ignore.clone(), home.index_path(&f.id), hub.clone());
        tokio::spawn(folder.run(rx));
    }

    if let Some(status) = status {
        let hub = hub.clone();
        tokio::spawn(async move {
            loop {
                let mut names: Vec<String> = hub.connected().iter().map(|id| hub.peer_name(id)).collect();
                names.sort();
                let mut text = if names.is_empty() {
                    "waiting for your devices".to_string()
                } else {
                    format!("connected to {}", names.join(", "))
                };
                match hub.invites.load(std::sync::atomic::Ordering::Relaxed) {
                    0 => {}
                    1 => text.push_str(" - 1 folder invitation"),
                    n => text.push_str(&format!(" - {n} folder invitations")),
                }
                *status.lock().unwrap() = text;
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });
    }
    tokio::spawn(net::dial_loop(hub.clone()));
    tokio::spawn(net::discovery(hub.clone()));
    tokio::select! {
        r = net::listen(hub.clone(), listener) => r,
        _ = tokio::signal::ctrl_c() => {
            log::info!("stopping");
            Ok(())
        }
    }
}

/// Watches a folder and sends one Scan after changes have been quiet for 2s.
fn watch(root: &PathBuf, tx: mpsc::Sender<FolderEvent>) -> Result<notify::RecommendedWatcher> {
    let (raw_tx, raw_rx) = std::sync::mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            let internal = ev.paths.iter().all(|p| {
                p.components().any(|c| c.as_os_str().to_string_lossy().starts_with(util::INTERNAL_PREFIX))
            });
            if !internal {
                let _ = raw_tx.send(());
            }
        }
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    std::thread::spawn(move || {
        while raw_rx.recv().is_ok() {
            while raw_rx.recv_timeout(Duration::from_secs(2)).is_ok() {}
            let _ = tx.try_send(FolderEvent::Scan);
        }
    });
    Ok(watcher)
}

/// Joins a folder another device shared with us. Used by the menu and `accept`.
pub fn accept_invite(cfg: &mut Config, inv: &config::Invite, path: PathBuf) -> Result<()> {
    if cfg.folders.iter().any(|f| f.id == inv.folder) {
        bail!("you already have this folder");
    }
    if let Some(f) = cfg.folders.iter().find(|f| f.path == path) {
        bail!("{} is already used by another synced folder ({})", path.display(), f.label());
    }
    cfg.folders.push(FolderConfig {
        id: inv.folder.clone(),
        path,
        ignore: config::default_ignores(),
        owner: Some(inv.from.clone()),
        access: inv.access,
        shared_with: vec![],
    });
    cfg.invites.retain(|i| !(i.from == inv.from && i.folder == inv.folder));
    Ok(())
}

fn absolute(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path);
    }
    Ok(std::env::current_dir()?.join(path))
}

pub fn default_device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "my-device".into())
}

#[cfg(windows)]
mod console {
    use std::os::windows::io::IntoRawHandle;

    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn AllocConsole() -> i32;
        fn FreeConsole() -> i32;
        fn SetConsoleTitleW(title: *const u16) -> i32;
        fn SetStdHandle(std_handle: u32, handle: *mut std::ffi::c_void) -> i32;
        fn GetConsoleScreenBufferInfo(h: *mut std::ffi::c_void, info: *mut BufferInfo) -> i32;
        fn GetLargestConsoleWindowSize(h: *mut std::ffi::c_void) -> Coord;
        fn SetConsoleScreenBufferSize(h: *mut std::ffi::c_void, size: Coord) -> i32;
        fn SetConsoleWindowInfo(h: *mut std::ffi::c_void, absolute: i32, rect: *const Rect) -> i32;
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Coord {
        x: i16,
        y: i16,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }

    #[repr(C)]
    #[derive(Default)]
    struct BufferInfo {
        size: Coord,
        cursor: Coord,
        attributes: u16,
        window: Rect,
        max_window: Coord,
    }

    /// Makes the menu window tall enough for the whole menu, with scrollback
    /// so new output always scrolls into view. Best effort: terminals that
    /// manage their own size (e.g. Windows Terminal) ignore it.
    unsafe fn size_window(h: *mut std::ffi::c_void) {
        let mut info = BufferInfo::default();
        if GetConsoleScreenBufferInfo(h, &mut info) == 0 {
            return;
        }
        let largest = GetLargestConsoleWindowSize(h);
        let cols = info.size.x.max(100).min(largest.x.max(80));
        let rows = 42.min(largest.y.max(25));
        SetConsoleScreenBufferSize(h, Coord { x: cols, y: 2000 });
        SetConsoleWindowInfo(h, 1, &Rect { left: 0, top: 0, right: cols - 1, bottom: rows - 1 });
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;

    /// The menu gets its own window (so it doesn't fight the terminal for
    /// input); plain commands print into the terminal they were typed in.
    pub fn attach(own_window: bool) {
        unsafe {
            if own_window {
                FreeConsole();
                AllocConsole();
                let title: Vec<u16> = "Mini Sync".encode_utf16().chain(Some(0)).collect();
                SetConsoleTitleW(title.as_ptr());
                // When started from the tray, the inherited input/output point
                // at "nothing" and the window would close at once. Point them
                // at the new window explicitly.
                let open = |name: &str| {
                    std::fs::OpenOptions::new().read(true).write(true).open(name).ok().map(|f| f.into_raw_handle())
                };
                if let Some(h) = open("CONIN$") {
                    SetStdHandle(STD_INPUT_HANDLE, h);
                }
                if let Some(h) = open("CONOUT$") {
                    SetStdHandle(STD_OUTPUT_HANDLE, h);
                    SetStdHandle(STD_ERROR_HANDLE, h);
                    size_window(h);
                }
            } else {
                AttachConsole(ATTACH_PARENT_PROCESS);
            }
        }
    }
}

#[cfg(not(windows))]
mod console {
    pub fn attach(_own_window: bool) {}
}
