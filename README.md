<p align="center">
  <img src="assets/preview-256.png" width="128" alt="Mini Sync icon">
</p>

<h1 align="center">Mini Sync</h1>

<p align="center">
  Keep a folder in sync between your own computers on the same network.<br>
  Windows &amp; macOS · one small program · no account, no cloud, no browser.
</p>

<p align="center">
  <a href="https://github.com/mk-manishkandel/MiniSync/releases/latest/download/minisync-windows-x64.exe"><b>⬇ Download for Windows</b></a>
  &nbsp;·&nbsp;
  <a href="https://github.com/mk-manishkandel/MiniSync/releases/latest/download/minisync-macos-arm64"><b>⬇ Mac (Apple chip)</b></a>
  &nbsp;·&nbsp;
  <a href="https://github.com/mk-manishkandel/MiniSync/releases/latest/download/minisync-macos-x64"><b>⬇ Mac (Intel)</b></a>
</p>

<p align="center">
  <a href="https://github.com/mk-manishkandel/MiniSync/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/mk-manishkandel/MiniSync?label=latest%20version"></a>
  <a href="https://github.com/mk-manishkandel/MiniSync/releases"><img alt="Downloads" src="https://img.shields.io/github/downloads/mk-manishkandel/MiniSync/total"></a>
</p>

---

Mini Sync is a small, personal take on [Syncthing](https://syncthing.net/): put files in the
`MiniSync` folder on one computer and they appear on your other computers. It's written from
scratch in Rust and keeps only what's needed for syncing between your own devices.

## Features

- **Two-way sync** of one or more folders between any number of your devices
- **You choose who gets what:** share each folder with only the devices you pick, each with
  **read & write** or **read only** access
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

Click the link for your computer. It always downloads the newest version:

| Computer | Download |
|---|---|
| Windows 10/11 (64-bit) | **[minisync-windows-x64.exe](https://github.com/mk-manishkandel/MiniSync/releases/latest/download/minisync-windows-x64.exe)** |
| Mac with Apple chip (M1/M2/M3/M4) | **[minisync-macos-arm64](https://github.com/mk-manishkandel/MiniSync/releases/latest/download/minisync-macos-arm64)** |
| Mac with Intel chip | **[minisync-macos-x64](https://github.com/mk-manishkandel/MiniSync/releases/latest/download/minisync-macos-x64)** |

That one file is the whole app: no installer needed. Open it and the setup starts (see [Set up](#set-up-on-every-device)).

Not sure which Mac you have? Apple menu → **About This Mac**: *Chip: Apple M…* means Apple chip,
*Processor: Intel…* means Intel.

Older versions, signatures (`.sig`) and checksums (`SHA256SUMS.txt`) are on the
[Releases page](https://github.com/mk-manishkandel/MiniSync/releases).

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

Do this with both computers on the same Wi-Fi. Every prompt says what **Enter** does, and Enter
never creates or shares anything by itself.

**On both devices**, open the downloaded file. Setup has 4 steps:

1. **Name** this device (e.g. `office-pc`, `macbook`).
2. It shows **this device's ID** (already copied to the clipboard). Send it to your other device,
   e.g. by email or chat to yourself.
3. **Share a folder from this device?**
   - On the device that **has the files**: type **1 (Yes)** and pick the folder.
   - On the **other** device: press **Enter (skip)**. It gets an invitation to join instead.
4. **Paste the other device's ID** and give that device a name (Enter = skip, add it later).
   If you shared a folder in step 3, choose what that device may do with it:
   **read & write**, **read only**, or Enter to decide later.

Adding a device later (menu → **2**) works the same way: after its ID and name, it asks for each of
your folders whether that device gets it, and with what access.

Setup ends with a short **What's next** list. Allow network access when asked
(Windows: firewall prompt → *Allow*; Mac: *Local Network* → *Allow*). Mini Sync then runs in the
background and starts at every login.

**Join the folder:** on the other device, once both are connected, the menu header shows
**✉ 1 folder invitation**. Open the menu → **6** → **Accept** → press Enter for the suggested
location (or type your own).

To check: the tray icon says **"connected to …"**. Put a file in the folder and it shows up on the
other device within seconds.

## Sharing and permissions

Every folder has an **owner**: the device that added it. Only the owner decides which devices get
the folder and what they may do:

| Access | The device… |
|---|---|
| **Read & write** | gets the files, and its changes are synced back to everyone |
| **Read only** | gets the files, but changes made there are **undone** (the edited copy is kept in its `.minisync-trash`) |
| **No access** | doesn't get the folder at all (the default for new devices and folders) |

- **Share or change access:** menu → **5 Share a folder / access** → pick a device → pick its access.
  The device gets an invitation the first time; later changes apply as soon as it reconnects.
- **Invitations:** menu → **6**. Accept (choose a location) or decline. Nothing is synced until you accept.
- **Taking access away** stops syncing on that device. Its copy of the files is kept.
- Devices sync a shared folder **through its owner**, so the owner must be on for changes to
  travel between the other devices. This keeps the owner's rules enforced: a read-only device can't
  slip changes in through another device.
- Different devices can share different folders: e.g. *Work* with your laptop only, and *Photos*
  read-only with a family computer.

Upgrading from v0.2: existing folders stay shared read & write with the devices you already had.
All devices need v0.3.0 or newer to talk to each other (they update automatically).

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

**Menu** (tray → *Open Mini Sync…*). Type a number; every action opens on its own page, and each
prompt says what **Enter** does:

```
  ╭────────────────────────────────────╮
  │  Mini Sync  v0.3.3                 │
  │  ● Running in the background       │
  │  ✉ 1 folder invitation - choose 6  │
  ╰────────────────────────────────────╯

  THIS DEVICE
    Name        office-pc
    ID          A4DQOBY-HA4DQOB-YHA4DQO-...
    Devices     laptop, family-mac

  FOLDERS
    MiniSync   C:\Users\me\MiniSync
               You share it with laptop (read & write), family-mac (read only)
    Photos     C:\Users\me\Photos
               Not shared with any device yet → choose 5
    Work       C:\Users\me\Work
               Shared with you by laptop · you can only read

  ────────────────────────────────────────────────────────────────

  DEVICES                          FOLDERS
    1  Copy this device's ID        4  Add a folder
    2  Add or update a device       5  Share a folder / access
    3  Remove a device              6  Invitations (1 new)
                                    7  Remove a folder
                                    8  Open a synced folder

  APP                              SECURITY & UPDATES
    9  View log                    12  Change this device's ID
   10  Restart                     13  Uninstall
   11  Stop until next login       14  Check for updates

    0  Close this window (syncing keeps running)
```

When a newer version is out, the header also shows **↑ Update vX available - choose 14**.

## Updates

Installed copies check GitHub for a new release **a minute after starting and every 6 hours**.
When there is one, the tray shows **"Install update vX.Y.Z"**. One click and Mini Sync:

1. downloads the file for your platform from the GitHub release,
2. **verifies its Ed25519 signature** against the public key built into the app
   (unsigned or modified files are refused),
3. swaps it in and restarts. Your settings, device ID and files are kept.

You can also update from the menu (*14*) or with `minisync update`.

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

**Uninstall** (menu 13) removes the program, start-at-login, settings and device key. Synced files and
`.minisync-trash` are kept.

## Troubleshooting

| Log says | Fix |
|---|---|
| `unknown device XXXX… tried to connect` | That device has your ID but you don't have its ID yet → menu 2 |
| `PC invites you to the folder ...` | Open the menu → 6 Invitations → Accept |
| `could not talk to X` | The other device runs an older Mini Sync. Update it (menu → 14) |
| A device doesn't get a folder | On the folder's owner: menu → 5 → give that device access. Then accept the invitation on the device (6) |
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
minisync add-folder PATH [--id ID]                 # a folder you own, shared with nobody yet
minisync share FOLDER-ID DEVICE read-write|read-only|none
minisync accept FOLDER-ID PATH | decline FOLDER-ID  # invitations (see `show`)
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
  ui.rs        terminal colours and layout for the menu
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
