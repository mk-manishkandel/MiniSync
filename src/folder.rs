//! One task per synced folder. It owns the folder's index and serialises
//! everything that changes it: local scans and applying remote changes
//! (Syncthing's scanner + puller, minus block-level transfers).

use crate::config::DeviceId;
use crate::net::{FolderEvent, Hub};
use crate::proto::{FileInfo, Kind, Message, Order};
use crate::scan::{self, Index};
use crate::util;
use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

const CHUNK: u32 = 1024 * 1024;
const INDEX_BATCH: usize = 1000;

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
    me: u64,
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
        Folder { id, root, ignores, index_path, hub, index, remote: HashMap::new(), complete: HashSet::new(), me }
    }

    pub async fn run(mut self, mut events: mpsc::Receiver<FolderEvent>) {
        if let Err(e) = self.prepare_root() {
            log::error!("[{}] {e:#}", self.id);
        }
        self.scan().await;
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
                                log::warn!("[{}] skipping {:?}: name not valid on this system", self.id, f.name);
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
        }
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
        match result {
            Ok(changed) if !changed.is_empty() => {
                let read_only = self.hub.rule(&self.id).and_then(|r| r.read_only_owner()).is_some();
                for f in &changed {
                    if read_only {
                        log::warn!("[{}] local {}: {} - this folder is read only, so it will be undone", self.id, describe(f), f.name);
                    } else {
                        log::info!("[{}] local {}: {}", self.id, describe(f), f.name);
                    }
                }
                self.save();
                self.broadcast(changed).await;
            }
            Ok(_) => {}
            Err(e) => log::error!("[{}] {e:#}", self.id),
        }
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

        let mut needs_scan = false;
        for (peer, r) in todo {
            let Some(action) = self.decide(&r, read_only.is_some()) else { continue };
            let result = match action {
                Action::Record(f) => {
                    self.index.insert(f.name.clone(), f.clone());
                    changed.push(f);
                    Ok(())
                }
                Action::Apply | Action::ApplyConflict => {
                    let conflict = matches!(action, Action::ApplyConflict);
                    if !self.disk_matches_index(&r.name) {
                        needs_scan = true; // local edit we have not scanned yet
                        continue;
                    }
                    self.apply(peer, &r, conflict, &mut changed).await
                }
            };
            if let Err(e) = result {
                log::warn!("[{}] {}: {e:#}", self.id, r.name);
            }
        }
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
                    Ok(()) => log::warn!("[{}] removed {} - added here, but this folder is read only (see {})", self.id, l.name, util::TRASH_DIR),
                    Err(e) => {
                        log::debug!("[{}] {}: {e:#}", self.id, l.name);
                        continue;
                    }
                }
            }
            self.index.remove(&l.name);
        }
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
            log::info!("[{}] deleted {} (from {from})", self.id, r.name);
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
            log::info!("[{}] received {} ({} bytes, from {from})", self.id, r.name, r.size);
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
