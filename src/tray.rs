//! Tray icon (Windows notification area / macOS menu bar) for the background
//! process, so you can see it is running, reach the menu and install updates.

use crate::config::Home;
use crate::service;
use crate::update::{self, SharedUpdate, UpdateState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// Text shown in the tray menu and tooltip, updated by the sync engine.
pub type Status = Arc<Mutex<String>>;

struct Tray {
    icon: TrayIcon,
    status: MenuItem,
    update: MenuItem,
    open: MenuItem,
    folder: MenuItem,
    log: MenuItem,
    quit: MenuItem,
    shown: (String, String),
}

fn build() -> anyhow::Result<Tray> {
    let status = MenuItem::new("Mini Sync - starting", false, None);
    let update = MenuItem::new("Check for updates", true, None);
    let open = MenuItem::new("Open Mini Sync...", true, None);
    let folder = MenuItem::new("Open synced folder", true, None);
    let log = MenuItem::new("View log", true, None);
    let quit = MenuItem::new("Quit (until next login)", true, None);
    let menu = Menu::new();
    menu.append(&status)?;
    menu.append(&update)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&open)?;
    menu.append(&folder)?;
    menu.append(&log)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit)?;

    #[cfg(windows)]
    let builder = TrayIconBuilder::new()
        .with_icon(Icon::from_rgba(include_bytes!("../assets/tray-32.rgba").to_vec(), 32, 32)?);
    #[cfg(target_os = "macos")]
    let builder = TrayIconBuilder::new()
        .with_icon_templated(Icon::from_rgba(include_bytes!("../assets/tray-template-44.rgba").to_vec(), 44, 44)?);

    let icon = builder.with_menu(Box::new(menu)).with_tooltip("Mini Sync").build()?;
    Ok(Tray { icon, status, update, open, folder, log, quit, shown: (String::new(), String::new()) })
}

fn update_label(state: &UpdateState) -> (String, bool) {
    match state {
        UpdateState::UpToDate => (format!("Check for updates (v{})", update::CURRENT_VERSION), true),
        UpdateState::Available(r) => (format!("Install update v{}", r.version), true),
        UpdateState::Working(msg) => (msg.clone(), false),
        UpdateState::Failed(msg) => {
            let short: String = msg.chars().take(60).collect();
            (format!("Update failed ({short}) - try again"), true)
        }
    }
}

/// Runs the tray on the main thread until "Quit" is chosen. Never returns.
pub fn run(home: Home, status: Status, updates: SharedUpdate) -> ! {
    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::new().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory); // no Dock icon
    }

    let exit_now = Arc::new(AtomicBool::new(false));
    let home = Arc::new(home);
    let mut tray: Option<Tray> = None;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500));

        // macOS requires the icon to be created once the loop is running.
        if let Event::NewEvents(StartCause::Init) = event {
            match build() {
                Ok(t) => tray = Some(t),
                Err(e) => log::warn!("tray icon unavailable: {e:#}"),
            }
        }
        if exit_now.load(Ordering::SeqCst) {
            tray = None; // removes the icon before exiting
            std::process::exit(0);
        }
        let Some(t) = tray.as_mut() else { return };

        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if ev.id == *t.open.id() {
                service::open_menu(&home);
            } else if ev.id == *t.folder.id() {
                if let Ok(cfg) = home.load_config() {
                    for f in &cfg.folders {
                        service::open_path(&f.path);
                    }
                }
            } else if ev.id == *t.log.id() {
                service::open_path(&service::log_path(&home));
            } else if ev.id == *t.update.id() {
                let state = updates.lock().unwrap().clone();
                let updates = updates.clone();
                let home = home.clone();
                let exit_now = exit_now.clone();
                match state {
                    UpdateState::Available(rel) => {
                        *updates.lock().unwrap() = UpdateState::Working(format!("Installing v{}...", rel.version));
                        std::thread::spawn(move || match update::install_from_background(&home, &rel) {
                            Ok(()) => {
                                log::info!("updated to v{}, restarting", rel.version);
                                exit_now.store(true, Ordering::SeqCst);
                            }
                            Err(e) => {
                                log::error!("update failed: {e:#}");
                                *updates.lock().unwrap() = UpdateState::Failed(format!("{e:#}"));
                            }
                        });
                    }
                    UpdateState::Working(_) => {}
                    _ => {
                        *updates.lock().unwrap() = UpdateState::Working("Checking for updates...".into());
                        std::thread::spawn(move || update::check_into(&updates));
                    }
                }
            } else if ev.id == *t.quit.id() {
                log::info!("quit from tray");
                tray = None;
                std::process::exit(0);
            }
        }

        let now = status.lock().unwrap().clone();
        let upd = updates.lock().unwrap().clone();
        let (label, enabled) = update_label(&upd);
        if (now.clone(), label.clone()) != t.shown {
            t.status.set_text(format!("Mini Sync - {now}"));
            t.update.set_text(&label);
            t.update.set_enabled(enabled);
            let extra = if matches!(upd, UpdateState::Available(_)) { " (update available)" } else { "" };
            let _ = t.icon.set_tooltip(Some(format!("Mini Sync - {now}{extra}")));
            t.shown = (now, label);
        }
    })
}
