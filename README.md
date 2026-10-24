<p align="center">
  <img src="assets/preview-256.png" width="128" alt="Mini Sync icon">
</p>

<h1 align="center">Mini Sync</h1>

<p align="center">
  Keep a folder in sync between your own computers on the same network.<br>
  Windows &amp; macOS · one small program · no account, no cloud, no browser.
</p>

---

Mini Sync is a small, personal take on [Syncthing](https://syncthing.net/): put files in the
`MiniSync` folder on one computer and they appear on your other computers. It's written from
scratch in Rust and keeps only what's needed for syncing between your own devices.

## Features

- **Two-way sync** of one or more folders between any number of your devices
- **Encrypted, trusted-only connections.** Each device has its own key, and devices only talk to IDs you added
- **Finds your devices automatically** on the local network (or use a fixed IP)
- **Runs in the background** with a tray / menu-bar icon, and starts at login
- **Safe by default:**
  - deleted or overwritten files go to `.minisync-trash`
  - edits made on both sides keep both copies (`*.sync-conflict-*`)
  - every download is checked by hash before it replaces anything
- **Automatic updates** from GitHub Releases, cryptographically signed
- Small (~4 MB) and light (~10 MB of memory)

## Download

Get the latest version from **[Releases](https://github.com/mk-manishkandel/MiniSync/releases/latest)**:

| Computer | File |
|---|---|
| Windows 10/11 (64-bit) | `minisync-windows-x64.exe` |
| Mac with Apple chip (M1/M2/M3/M4) | `minisync-macos-arm64` |
| Mac with Intel chip | `minisync-macos-x64` |

That one file is the whole app.

<details>
<summary><b>Windows says "Windows protected your PC"</b></summary>

The app isn't code-signed with a paid certificate, so the first time you open a downloaded copy
Windows SmartScreen warns you. Click **More info → Run anyway**. Updates installed by the
app itself don't show this warning.
</details>

<details>
<summary><b>Mac says the app "cannot be opened"</b></summary>

Open Terminal in the folder where you downloaded it and run (use your file's name):

```sh
chmod +x minisync-macos-arm64
xattr -d com.apple.quarantine minisync-macos-arm64
./minisync-macos-arm64
```
After the first setup, Mini Sync installs itself and updates on its own.
</details>

## Set up (on every device)

Do this with both computers on the same Wi-Fi.

1. **Open the downloaded file.** The setup window asks for:
   - a **name** for this device (e.g. `office-pc`, `macbook`)
   - the **folder to sync**: press Enter for the default `MiniSync` folder in your home folder
2. It shows **this device's ID** (already copied to the clipboard).
3. It asks for the **other device's ID**. Paste it and give that device a name.
   Send IDs to yourself (email, chat, notes) to copy them between computers.
4. **Allow network access** when asked:
   - Windows: firewall prompt → *Allow*
   - Mac: *Local Network* → *Allow*
5. Done. It installs itself, runs in the background and starts at every login.

Each device must have **the other device's ID**, and synced folders must use the **same folder ID**
on every device. The default folder ID `minisync` is used automatically, so if you keep the defaults
everything matches.

To check: the tray icon should say **"connected to …"**. Put a file in `MiniSync` and it
shows up on the other device within seconds.

## Everyday use

Just use the `MiniSync` folder. Changes sync whenever the devices are on the same network.
If one is away, changes wait and sync when it's back.

**Tray icon** (Windows: by the clock, maybe under **^**; Mac: top menu bar):

| Item | What it does |
|---|---|
| *Mini Sync - connected to …* | Status |
| *Check for updates* / *Install update vX* | See [Updates](#updates) |
| *Open Mini Sync…* | Opens the menu below |
| *Open synced folder* · *View log* | Shortcuts |
| *Quit* | Stops until next login |

**Menu** (tray → *Open Mini Sync…*). Every question accepts **Enter / 0 = back**:

```
 1) Copy this device's ID            7) View log
 2) Add or update a device           8) Restart
 3) Remove a device                  9) Stop (until next login)
 4) Add another folder              10) Change this device's ID
 5) Remove a folder (files stay)    11) Uninstall
 6) Open synced folder              12) Check for updates
```

## Updates

Installed copies check GitHub for a new release **a minute after starting and every 6 hours**.
When there is one, the tray shows **"Install update vX.Y.Z"**. One click and Mini Sync:

1. downloads the file for your platform from the GitHub release,
2. **verifies its Ed25519 signature** against the public key built into the app
   (unsigned or modified files are refused),
3. swaps it in and restarts. Your settings, device ID and files are kept.

You can also update from the menu (*12*) or with `minisync update`.

## How it works

```
 Windows PC                        Wi-Fi                        Mac
 MiniSync folder  ◄──── encrypted (Noise protocol) ────►  MiniSync folder
 device ID AAAA…                                           device ID BBBB…
```

1. **Identity:** each device generates a key pair; the **device ID is its public key**.
   Connections use the Noise protocol (`XX_25519_ChaChaPoly_BLAKE2s`). Both sides prove their
   key, and anything not in your device list is rejected.
2. **Discovery:** every 20 s each device broadcasts "I'm ID X on port 22100" on the LAN (UDP 21100).
   Fixed addresses can be added for networks that block broadcast.
3. **Index:** each device scans its folder (SHA-256 per file) and keeps a list of every file with a
   **version vector**: a counter per device that changed it. On connect, devices swap lists;
   after that only changes are sent.
4. **Deciding:** for each file, newer version → download it; same → nothing;
   changed on both sides → conflict: both copies kept, and the loser is renamed `*.sync-conflict-<date>-<device>.*`.
5. **Applying:** downloads go to `.minisync-tmp`, are hash-checked, then moved into place. Replaced
   or deleted files go to `.minisync-trash`.
6. **Watching:** file-system events trigger a rescan within ~2 s; a full rescan runs every 5 minutes.

A hidden `.minisync-folder` marker in each synced folder protects against an unplugged drive or a moved
folder being mistaken for "everything was deleted".

## Security

- **Lost or stolen device:** on each of your *other* devices: menu → **Remove a device**. They refuse it from then on.
- **Key may have been copied (you still have the device):** menu → **Change this device's ID**, then on the
  other devices: menu → **Add or update a device** → paste the new ID under the **same name**.
- Only devices in your list can connect, and all traffic is encrypted.
- Updates must be signed with the project's release key. The private key never goes into this repository.

## Where things are

| | Windows | Mac |
|---|---|---|
| Synced files (default) | `%USERPROFILE%\MiniSync` | `~/MiniSync` |
| Installed program | `%LOCALAPPDATA%\MiniSync\minisync.exe` | `~/Library/Application Support/MiniSync/minisync` |
| Settings, device key, log | `%APPDATA%\minisync\` | `~/Library/Application Support/minisync/` |
| Start at login | Registry `HKCU\…\Run\MiniSync` | `~/Library/LaunchAgents/com.minisync.plist` |

**Uninstall** (menu 11) removes the program, start-at-login, settings and device key. Synced files and
`.minisync-trash` are kept.

## Troubleshooting

| Log says | Fix |
|---|---|
| `unknown device XXXX… tried to connect` | That device has your ID but you don't have its ID yet → menu 2 |
| `X shares folder "a" which is not configured here` | Folder IDs differ between devices → remove the folder (5) and add it again (4) with the **same ID** as the other device |
| `already running (port 22100 is in use)` | Only one copy runs per computer; use the tray icon |
| nothing connects | Same Wi-Fi? Firewall / Local Network allowed? Try a fixed address: `minisync add-peer NAME ID --address 192.168.x.x:22100` |

## Command line

Everything in the menu is also available as commands:

```
minisync                       # setup / menu
minisync status | start | stop | update
minisync id                    # this device's ID
minisync add-peer NAME ID [--address IP:22100]   # same NAME replaces its ID
minisync remove-peer NAME
minisync add-folder ID PATH
minisync remove-folder ID      # files are kept
minisync new-id                # new device ID
minisync show                  # print settings
minisync run                   # sync in this terminal with logs on screen
minisync install | uninstall [--keep-settings]
```

## Building from source

Requires [Rust](https://rustup.rs).

```sh
cargo build --release      # → target/release/minisync(.exe)
cargo test
```

- **Mac:** also run `xcode-select --install` once.
- **Windows:** the MSVC toolchain works out of the box. The GNU toolchain needs MinGW (`windres`) for the icon.

## Releasing an update

1. Change `version` in `Cargo.toml` (e.g. `0.2.0` → `0.2.1`).
2. Commit and push to `main`.

The [Release workflow](.github/workflows/release.yml) then tests, builds Windows + macOS (Apple chip
and Intel), signs every file, and publishes release `v0.2.1`. Installed copies offer the update
within 6 hours, or right away with *Check for updates*. Pushes that don't change the version build
nothing.

### Signing key (one-time setup)

- The public key is in `release-public-key.txt` and is built into the app.
- The private key must be stored as a repository secret named **`RELEASE_SIGNING_KEY`**:
  *Settings → Secrets and variables → Actions → New repository secret*.
- Keep a private backup of the key file. If it is lost, future updates can't be signed, and
  every device would need a new key pair and a manual reinstall.

To create a new key pair:

```sh
cargo run --release --manifest-path tools/release-sign/Cargo.toml -- keygen ~/minisync-release-signing-key.txt
```
The command prints the public key; put it in `release-public-key.txt`.

## Project layout

```
src/
  main.rs      command line, startup, single-instance lock
  menu.rs      setup wizard and menu
  tray.rs      tray / menu-bar icon
  service.rs   install, start-at-login, start/stop
  update.rs    update check, signature verification, self-replace
  net.rs       encrypted connections, LAN discovery, file requests
  folder.rs    per-folder sync: pull, apply, conflicts, trash
  scan.rs      folder scanning and the file index
  proto.rs     messages, file info, version vectors
  config.rs    settings, device ID, keys
  util.rs      paths, ignore patterns, hashing
assets/        icons (source + generated .ico/.icns/tray)
tools/
  make-icons/    regenerates icons from assets/icon-source.png
  release-sign/  release key generation and signing
.github/workflows/release.yml
```

## Limits

- Whole files are transferred (no partial/delta sync), so huge files that change a little are slow to re-sync.
- Symlinks and file permissions are not synced.
- Names Windows can't store (e.g. `:` `?` from a Mac) are skipped with a warning.
- Same network or a VPN (e.g. Tailscale) only; no relays.
