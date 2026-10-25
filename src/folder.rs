//! One task per synced folder. It owns the folder's index and serialises
//! everything that changes it: local scans and applying remote changes
//! (Syncthing's scanner + puller, minus block-level transfers).

use crate::config::DeviceId;
use crate::net::{FolderEvent, Hub};
use crate::proto::{FileInfo, Kind, Message, Order};
use crate::scan::{self, Index};
use crate::util;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

const CHUNK: u32 = 1024 * 1024;
const INDEX_BATCH: usize = 1000;
/// Per scan or pull, at most this many files are listed in the log one by one.
const DETAIL_LINES: usize = 20;

pub struct Folder {
    pub id: String,
    pub root: PathBuf,
    pub ignores: Vec<String>,
    pub index_path: PathBuf,
    pub hub: Arc<Hub>,
    index: Index,
    /// Latest file list each connected peer has told us about.
    remote: HashMap<DeviceId, HashMap<String, FileInfo>>,
    /// Peers whose full file list we have received since they connected.
    complete: HashSet<DeviceId>,
    /// File lines logged in the current scan or pull (see `note`).
    noted: usize,
    me: u64,
    /// Why the folder can't sync right now (missing folder or marker).
    error: Option<String>,
    /// Changes from the last pull that could not be applied yet.
    not_received: u64,
}

/// What a folder is doing, shown in the menu and the tray. The background
/// process keeps it in the hub and writes it to `status.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FolderStatus {
    pub state: SyncState,
    /// Devices this folder syncs with that are not connected.
    pub waiting_for: Vec<String>,
    /// While receiving: progress of the current round.
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Connected devices still missing some of our files: (name, files, bytes).
    pub behind: Vec<(String, u64, u64)>,
    pub not_received: u64,
    pub error: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncState {
    #[default]
    Scanning,
    /// None of the devices it syncs with is connected.
    Waiting,
    /// Connected, still getting the other device's file list.
    Comparing,
    Receiving,
    /// Another device is still getting files from us.
    Sending,
    /// Some received changes could not be applied (see the log).
    Incomplete,
    UpToDate,
    Error,
}

enum Action {
    /// Take the remote entry as-is (or merged) without touching disk.
    Record(FileInfo),
    Apply,
    /// Both sides changed it; remote wins, so move ours aside first.
    ApplyConflict,
}

impl Folder {
    pub fn new(id: String, root: PathBuf, ignores: Vec<String>, index_path: PathBuf, hub: Arc<Hub>) -> Folder {
        let index = scan::load_index(&index_path);
        let me = hub.me.short();
        Folder {
            id,
            root,
            ignores,
            index_path,
            hub,
            index,
            remote: HashMap::new(),
            complete: HashSet::new(),
            noted: 0,
            me,
            error: None,
            not_received: 0,
        }
    }

    pub async fn run(mut self, mut events: mpsc::Receiver<FolderEvent>) {
        self.set_status(FolderStatus::default());
        if let Err(e) = self.prepare_root() {
            log::error!("[{}] {e:#}", self.id);
        }
        self.scan().await;
        self.refresh_status();
        let mut pull_tick = tokio::time::interval(Duration::from_secs(60));
        let mut scan_tick = tokio::time::interval(Duration::from_secs(300));
        pull_tick.tick().await;
        scan_tick.tick().await;
        loop {
            tokio::select! {
                ev = events.recv() => match ev {
                    None => return,
                    Some(FolderEvent::Scan) => { self.scan().await; self.pull().await; }
                    Some(FolderEvent::PeerConnected(p)) => self.send_full_index(p).await,
                    Some(FolderEvent::PeerDisconnected(p)) => { self.remote.remove(&p); self.complete.remove(&p); }
                    Some(FolderEvent::RemoteIndex(p, files, complete)) => {
                        if complete {
                            self.complete.insert(p);
                        }
                        let known = self.remote.entry(p).or_default();
                        for f in files {
                            if util::safe_join(&self.root, &f.name).is_none() {
                                if crate::logfile::throttle(&format!("name-{}-{}", self.id, f.name), crate::logfile::REPEAT) {
                                    log::warn!("[{}] skipping {:?}: name not valid on this system", self.id, f.name);
                                }
                                continue;
                            }
                            known.insert(f.name.clone(), f);
                        }
                        self.pull().await;
                    }
                },
                _ = pull_tick.tick() => self.pull().await,
                _ = scan_tick.tick() => { self.scan().await; self.pull().await; }
            }
            self.refresh_status();
        }
    }

    fn set_status(&self, s: FolderStatus) {
        self.hub.status.lock().unwrap().insert(self.id.clone(), s);
    }

    fn update_status(&self, f: impl FnOnce(&mut FolderStatus)) {
        if let Some(s) = self.hub.status.lock().unwrap().get_mut(&self.id) {
            f(s);
        }
    }

    /// Works out the folder's state when it is not in the middle of receiving.
    fn refresh_status(&self) {
        let mut s = FolderStatus { not_received: self.not_received, ..Default::default() };
        if let Some(e) = &self.error {
            s.state = SyncState::Error;
            s.error = e.clone();
            return self.set_status(s);
        }
        let Some(rule) = self.hub.rule(&self.id) else { return };
        let devices: Vec<DeviceId> = match rule.owner {
            Some(o) => vec![o],
            None => rule.shares.keys().copied().collect(),
        };
        let connected = self.hub.connected();
        let (online, offline): (Vec<DeviceId>, Vec<DeviceId>) = devices.into_iter().partition(|d| connected.contains(d));
        s.waiting_for = offline.iter().map(|d| self.hub.peer_name(d)).collect();
        s.waiting_for.sort();
        for peer in &online {
            // Only devices that may send us changes report what they have.
            if !rule.accepts_from(peer) || !self.complete.contains(peer) || !rule.can_send_to(peer) {
                continue;
            }
            let theirs = self.remote.get(peer);
            let (mut files, mut bytes) = (0, 0);
            for l in self.index.values() {
                let missing = match theirs.and_then(|t| t.get(&l.name)) {
                    None => !l.deleted,
                    Some(r) => l.version.compare(&r.version) == Order::Greater && !l.same_content(r),
                };
                if missing {
                    files += 1;
                    if !l.deleted {
                        bytes += l.size;
                    }
                }
            }
            if files > 0 {
                s.behind.push((self.hub.peer_name(peer), files, bytes));
            }
        }
        s.state = if online.is_empty() {
            SyncState::Waiting
        } else if online.iter().any(|p| rule.accepts_from(p) && !self.complete.contains(p)) {
            SyncState::Comparing
        } else if !s.behind.is_empty() {
            SyncState::Sending
        } else if self.not_received > 0 {
            SyncState::Incomplete
        } else {
            SyncState::UpToDate
        };
        self.set_status(s);
    }

    fn prepare_root(&self) -> Result<()> {
        let marker = self.root.join(util::MARKER);
        if marker.exists() {
            return Ok(());
        }
        if !self.index.is_empty() {
            bail!(
                "{} is missing its {} marker; refusing to sync it (if you moved it on purpose, create an empty {} file there)",
                self.root.display(),
                util::MARKER,
                util::MARKER
            );
        }
        std::fs::create_dir_all(&self.root).with_context(|| format!("creating {}", self.root.display()))?;
        std::fs::write(&marker, b"")?;
        hide(&marker);
        Ok(())
    }

    async fn scan(&mut self) {
        let root = self.root.clone();
        let ignores = self.ignores.clone();
        let me = self.me;
        let mut index = std::mem::take(&mut self.index);
        let res = tokio::task::spawn_blocking(move || {
            let r = scan::scan(&root, &ignores, &mut index, me);
            (index, r)
        })
        .await;
        let (index, result) = match res {
            Ok(x) => x,
            Err(e) => {
                log::error!("[{}] scan task failed: {e}", self.id);
                return;
            }
        };
        self.index = index;
        self.error = result.as_ref().err().map(|e| format!("{e:#}"));
        match result {
            Ok(changed) if !changed.is_empty() => {
                let read_only = self.hub.rule(&self.id).and_then(|r| r.read_only_owner()).is_some();
                for f in &changed {
                    if read_only {
                        self.note(true, format!("[{}] local {}: {} - this folder is read only, so it will be undone", self.id, describe(f), f.name));
                    } else {
                        self.note(false, format!("[{}] local {}: {}", self.id, describe(f), f.name));
                    }
                }
                self.end_round("local changes");
                self.save();
                self.broadcast(changed).await;
            }
            Ok(_) => {}
            Err(e) => log::error!("[{}] {e:#}", self.id),
        }
    }

    /// Logs one file line, unless this round already listed `DETAIL_LINES` of them.
    fn note(&mut self, warn: bool, line: String) {
        if self.noted < DETAIL_LINES {
            if warn {
                log::warn!("{line}");
            } else {
                log::info!("{line}");
            }
        }
        self.noted += 1;
    }

    /// Ends a round of `note`s with one summary line for the files not listed.
    fn end_round(&mut self, what: &str) {
        if self.noted > DETAIL_LINES {
            log::info!("[{}] ...and {} more {what} (not listed)", self.id, self.noted - DETAIL_LINES);
        }
        self.noted = 0;
    }

    fn may_send_to(&self, peer: &DeviceId) -> bool {
        self.hub.rule(&self.id).is_some_and(|r| r.can_send_to(peer))
    }

    async fn send_full_index(&self, peer: DeviceId) {
        if !self.may_send_to(&peer) {
            return;
        }
        let files: Vec<FileInfo> = self.index.values().cloned().collect();
        let batches: Vec<&[FileInfo]> = if files.is_empty() { vec![&[]] } else { files.chunks(INDEX_BATCH).collect() };
        let last = batches.len() - 1;
        for (i, batch) in batches.into_iter().enumerate() {
            let msg = Message::Index { folder: self.id.clone(), files: batch.to_vec(), complete: i == last };
            self.hub.send(peer, msg).await;
        }
    }

    async fn broadcast(&self, files: Vec<FileInfo>) {
        for peer in self.hub.connected().into_iter().filter(|p| self.may_send_to(p)) {
            for batch in files.chunks(INDEX_BATCH) {
                self.hub.send(peer, Message::Index { folder: self.id.clone(), files: batch.to_vec(), complete: false }).await;
            }
        }
    }

    fn save(&self) {
        if let Err(e) = scan::save_index(&self.index_path, &self.index) {
            log::error!("[{}] saving index: {e}", self.id);
        }
    }

    fn decide(&self, r: &FileInfo, read_only: bool) -> Option<Action> {
        let Some(l) = self.index.get(&r.name) else {
            return Some(if r.deleted { Action::Record(r.clone()) } else { Action::Apply });
        };
        if read_only {
            // The owner's copy always wins; a local edit is replaced (and kept in the trash).
            return match (l.same_content(r), l.version == r.version) {
                (true, true) => None,
                (true, false) => Some(Action::Record(r.clone())),
                (false, _) => Some(Action::Apply),
            };
        }
        match r.version.compare(&l.version) {
            Order::Equal | Order::Lesser => None,
            Order::Greater if l.same_content(r) => Some(Action::Record(r.clone())),
            Order::Greater => Some(Action::Apply),
            Order::Concurrent if l.same_content(r) => {
                let mut m = r.clone();
                m.version = l.version.merged(&r.version);
                Some(Action::Record(m))
            }
            Order::Concurrent if r.wins_conflict(l) => Some(Action::ApplyConflict),
            Order::Concurrent => None, // we win; the peer will move its copy aside
        }
    }

    /// Brings the local folder up to date with everything peers have announced.
    async fn pull(&mut self) {
        if !self.root.join(util::MARKER).exists() {
            return;
        }
        let Some(rule) = self.hub.rule(&self.id) else { return };
        let read_only = rule.read_only_owner();
        // Newest announced version of each name across all peers.
        let mut wanted: HashMap<String, (DeviceId, FileInfo)> = HashMap::new();
        for (peer, files) in self.remote.iter().filter(|(p, _)| rule.accepts_from(p)) {
            for f in files.values() {
                if self.decide(f, read_only.is_some()).is_none() {
                    continue;
                }
                match wanted.get(&f.name) {
                    Some((_, cur)) if f.version.compare(&cur.version) != Order::Greater => {}
                    _ => {
                        wanted.insert(f.name.clone(), (*peer, f.clone()));
                    }
                }
            }
        }
        let mut changed = Vec::new();
        if let Some(owner) = read_only {
            self.remove_local_extras(owner);
        }
        if wanted.is_empty() {
            self.not_received = 0;
            return;
        }
        let mut todo: Vec<(DeviceId, FileInfo)> = wanted.into_values().collect();
        // Deletes deepest-first, then directories parent-first, then files.
        todo.sort_by(|(_, a), (_, b)| {
            let rank = |f: &FileInfo| if f.deleted { 0 } else if f.kind == Kind::Dir { 1 } else { 2 };
            rank(a).cmp(&rank(b)).then_with(|| {
                if a.deleted { b.name.cmp(&a.name) } else { a.name.cmp(&b.name) }
            })
        });

        let size_of = |f: &FileInfo| if f.deleted || f.kind != Kind::File { 0 } else { f.size };
        let files_total = todo.len() as u64;
        let bytes_total: u64 = todo.iter().map(|(_, f)| size_of(f)).sum();
        self.update_status(|s| {
            s.state = SyncState::Receiving;
            (s.files_done, s.files_total, s.bytes_done, s.bytes_total) = (0, files_total, 0, bytes_total);
        });

        let mut needs_scan = false;
        let mut not_received = 0;
        let (mut files_done, mut bytes_done) = (0, 0);
        let mut last_flush = Instant::now();
        for (peer, r) in todo {
            if let Some(action) = self.decide(&r, read_only.is_some()) {
                let result = match action {
                    Action::Record(f) => {
                        self.index.insert(f.name.clone(), f.clone());
                        changed.push(f);
                        Ok(())
                    }
                    Action::Apply | Action::ApplyConflict if !self.disk_matches_index(&r.name) => {
                        needs_scan = true; // local edit we have not scanned yet
                        Ok(())
                    }
                    Action::Apply | Action::ApplyConflict => {
                        let conflict = matches!(action, Action::ApplyConflict);
                        self.apply(peer, &r, conflict, &mut changed).await
                    }
                };
                if let Err(e) = result {
                    not_received += 1;
                    if crate::logfile::throttle(&format!("pull-{}-{}", self.id, r.name), crate::logfile::REPEAT) {
                        log::warn!("[{}] {}: {e:#}", self.id, r.name);
                    }
                }
            }
            files_done += 1;
            bytes_done += size_of(&r);
            self.update_status(|s| (s.files_done, s.bytes_done) = (files_done, bytes_done));
            // Tell the other devices what we have so far, so their view of
            // the transfer moves too (and a restart resumes from here).
            if last_flush.elapsed() >= Duration::from_secs(3) && !changed.is_empty() {
                self.save();
                self.broadcast(std::mem::take(&mut changed)).await;
                last_flush = Instant::now();
            }
        }
        self.not_received = not_received;
        self.end_round("received changes");
        if !changed.is_empty() {
            self.save();
            self.broadcast(changed).await;
        }
        if needs_scan {
            self.scan().await;
        }
    }

    /// Read-only folder: files added here that the owner doesn't have are
    /// moved to the trash. Only once the owner's full list has arrived, so a
    /// file isn't mistaken for "extra" while the list is still coming in.
    fn remove_local_extras(&mut self, owner: DeviceId) {
        if !self.complete.contains(&owner) {
            return;
        }
        let Some(theirs) = self.remote.get(&owner) else { return };
        let mut extras: Vec<FileInfo> =
            self.index.values().filter(|l| !theirs.contains_key(&l.name)).cloned().collect();
        if extras.is_empty() {
            return;
        }
        // Deepest first, so a folder's files go before the folder itself.
        extras.sort_by(|a, b| b.name.cmp(&a.name));
        for l in extras {
            if !l.deleted {
                if !self.disk_matches_index(&l.name) {
                    continue; // changed again since the last scan; next round
                }
                let Some(path) = util::safe_join(&self.root, &l.name) else { continue };
                let result = match l.kind {
                    Kind::File => self.trash(&path, &l.name),
                    // Only if empty: anything left inside (e.g. ignored files) stays.
                    Kind::Dir => std::fs::remove_dir(&path).map_err(Into::into),
                };
                match result {
                    Ok(()) => self.note(true, format!("[{}] removed {} - added here, but this folder is read only (see {})", self.id, l.name, util::TRASH_DIR)),
                    Err(e) => {
                        log::debug!("[{}] {}: {e:#}", self.id, l.name);
                        continue;
                    }
                }
            }
            self.index.remove(&l.name);
        }
        self.end_round("files removed");
        self.save();
    }

    /// True if what is on disk is exactly what our index says (so overwriting
    /// it cannot lose an edit we have not seen yet).
    fn disk_matches_index(&self, name: &str) -> bool {
        let Some(path) = util::safe_join(&self.root, name) else { return false };
        let meta = std::fs::symlink_metadata(&path).ok();
        match (self.index.get(name), meta) {
            (None, None) => true,
            (None, Some(_)) => false,
            (Some(l), None) => l.deleted,
            (Some(l), Some(m)) => {
                if l.deleted {
                    return false;
                }
                match l.kind {
                    Kind::Dir => m.is_dir(),
                    Kind::File => m.is_file() && m.len() == l.size && util::mtime_secs(&m) == l.mtime,
                }
            }
        }
    }

    async fn apply(&mut self, peer: DeviceId, r: &FileInfo, conflict: bool, changed: &mut Vec<FileInfo>) -> Result<()> {
        let path = util::safe_join(&self.root, &r.name).ok_or_else(|| anyhow!("invalid name"))?;
        let from = self.hub.peer_name(&peer);

        if conflict && path.is_file() {
            let target = conflict_path(&path, &self.hub.me);
            std::fs::rename(&path, &target)?;
            log::warn!("[{}] conflict on {}: kept our version as {}", self.id, r.name, target.display());
        }

        if r.deleted {
            if path.is_dir() {
                if std::fs::remove_dir(&path).is_err() {
                    // Not empty: something here the peer has not seen. Keep the
                    // directory and make our copy the newest so it comes back there.
                    let l = self.index.get(&r.name).cloned().unwrap_or_else(|| r.clone());
                    let mut keep = l;
                    keep.deleted = false;
                    keep.kind = Kind::Dir;
                    keep.version = keep.version.merged(&r.version).bumped(self.me);
                    keep.modified_by = self.me;
                    self.index.insert(r.name.clone(), keep.clone());
                    changed.push(keep);
                    return Ok(());
                }
            } else if path.exists() {
                self.trash(&path, &r.name)?;
            }
            self.note(false, format!("[{}] deleted {} (from {from})", self.id, r.name));
        } else if r.kind == Kind::Dir {
            if path.is_file() {
                self.trash(&path, &r.name)?;
            }
            std::fs::create_dir_all(&path)?;
        } else {
            let tmp = self.download(peer, r).await?;
            if path.is_dir() {
                self.trash(&path, &r.name)?;
            } else if path.exists() {
                self.trash(&path, &r.name)?;
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(&tmp, &path).with_context(|| format!("moving into place {}", path.display()))?;
            self.note(false, format!("[{}] received {} ({} bytes, from {from})", self.id, r.name, r.size));
        }
        self.index.insert(r.name.clone(), r.clone());
        changed.push(r.clone());
        Ok(())
    }

    /// Downloads the whole file into the temp dir, verifies its hash and
    /// stamps the remote mtime on it. Returns the temp path.
    async fn download(&self, peer: DeviceId, r: &FileInfo) -> Result<PathBuf> {
        let tmp_dir = self.root.join(util::TMP_DIR);
        if !tmp_dir.exists() {
            std::fs::create_dir_all(&tmp_dir)?;
            hide(&tmp_dir);
        }
        let tmp = tmp_dir.join(format!("{}.part", &util::hex(&r.hash)[..16]));
        let result = async {
            let mut file = tokio::fs::File::create(&tmp).await?;
            let mut hasher = Sha256::new();
            let mut offset = 0u64;
            while offset < r.size {
                let want = CHUNK.min((r.size - offset) as u32);
                let data = self.hub.request(peer, &self.id, &r.name, offset, want).await?;
                if data.is_empty() {
                    bail!("peer's file is shorter than announced (changed while sending?)");
                }
                hasher.update(&data);
                file.write_all(&data).await?;
                offset += data.len() as u64;
                self.update_status(|s| s.bytes_done += data.len() as u64);
            }
            file.flush().await?;
            file.sync_all().await?;
            drop(file);
            let got: [u8; 32] = hasher.finalize().into();
            if got != r.hash || offset != r.size {
                bail!("content does not match (file changed on the peer while sending?), will retry");
            }
            let t = filetime::FileTime::from_unix_time(r.mtime, 0);
            filetime::set_file_mtime(&tmp, t)?;
            Ok(())
        }
        .await;
        if let Err(e) = result {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(tmp)
    }

    /// Instead of deleting or overwriting, keep the old copy in .minisync-trash.
    fn trash(&self, path: &Path, name: &str) -> Result<()> {
        let trash = self.root.join(util::TRASH_DIR);
        if !trash.exists() {
            std::fs::create_dir_all(&trash)?;
            hide(&trash);
        }
        let dest = trash.join(stamped(name, &timestamp()));
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(path, &dest).with_context(|| format!("moving {} to trash", path.display()))?;
        Ok(())
    }
}

fn describe(f: &FileInfo) -> &'static str {
    match (f.deleted, f.kind) {
        (true, _) => "deleted",
        (false, Kind::Dir) => "dir",
        (false, Kind::File) => "changed",
    }
}

/// "notes.txt" -> "notes.sync-conflict-20261024-153000-ABCDEFG.txt"
fn conflict_path(path: &Path, me: &DeviceId) -> PathBuf {
    let file = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tag = format!("sync-conflict-{}-{}", timestamp(), &me.to_string()[..7]);
    path.with_file_name(insert_before_ext(file, &tag))
}

fn stamped(name: &str, ts: &str) -> PathBuf {
    let mut parts: Vec<&str> = name.split('/').collect();
    let last = parts.pop().unwrap_or("file");
    let mut p: PathBuf = parts.iter().collect();
    p.push(insert_before_ext(last, ts));
    p
}

fn insert_before_ext(file: &str, tag: &str) -> String {
    match file.rfind('.') {
        Some(i) if i > 0 => format!("{}.{}{}", &file[..i], tag, &file[i..]),
        _ => format!("{file}.{tag}"),
    }
}

/// Local-agnostic UTC timestamp "YYYYMMDD-HHMMSS" without pulling in a date crate.
fn timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Howard Hinnant's civil-from-days.
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(windows)]
fn hide(path: &Path) {
    let _ = std::process::Command::new("attrib").arg("+h").arg(path).output();
}

#[cfg(not(windows))]
fn hide(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(insert_before_ext("a.txt", "X"), "a.X.txt");
        assert_eq!(insert_before_ext(".bashrc", "X"), ".bashrc.X");
        assert_eq!(insert_before_ext("Makefile", "X"), "Makefile.X");
        assert_eq!(stamped("a/b/c.txt", "T"), PathBuf::from("a").join("b").join("c.T.txt"));
        assert_eq!(timestamp().len(), 15);
    }
}
