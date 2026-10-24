use crate::proto::{FileInfo, Kind};
use crate::util;
use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use walkdir::WalkDir;

pub type Index = HashMap<String, FileInfo>;

/// Walks the folder, compares it with the stored index and records every change
/// with a bumped version. Returns only the entries that changed.
pub fn scan(root: &Path, ignores: &[String], index: &mut Index, me: u64) -> Result<Vec<FileInfo>> {
    if !root.join(util::MARKER).exists() {
        bail!(
            "{} is missing {} (folder unmounted or moved?) - not scanning",
            root.display(),
            util::MARKER
        );
    }

    let mut changed = Vec::new();
    let mut seen = HashSet::new();

    let walker = WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !e.file_name().to_str().map_or(true, |n| util::is_ignored(ignores, n)));

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                log::warn!("scan: {e}");
                continue;
            }
        };
        let ft = entry.file_type();
        if ft.is_symlink() {
            continue;
        }
        let Some(name) = util::rel_name(root, entry.path()) else { continue };
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                log::warn!("scan {name}: {e}");
                continue;
            }
        };
        seen.insert(name.clone());
        let old = index.get(&name);

        if ft.is_dir() {
            if old.map_or(true, |o| o.deleted || o.kind != Kind::Dir) {
                let fi = new_version(old, name.clone(), Kind::Dir, 0, 0, [0; 32], me);
                index.insert(name, fi.clone());
                changed.push(fi);
            }
            continue;
        }

        let size = meta.len();
        let mtime = util::mtime_secs(&meta);
        if let Some(o) = old {
            if !o.deleted && o.kind == Kind::File && o.size == size && o.mtime == mtime {
                continue; // unchanged
            }
        }
        let hash = match util::hash_file(entry.path()) {
            Ok(h) => h,
            Err(e) => {
                log::warn!("hash {name}: {e}");
                continue;
            }
        };
        if let Some(o) = old {
            if !o.deleted && o.kind == Kind::File && o.hash == hash {
                // Only the timestamp moved; keep the version, just remember the new mtime.
                let mut fi = o.clone();
                fi.mtime = mtime;
                index.insert(name, fi);
                continue;
            }
        }
        let fi = new_version(old, name.clone(), Kind::File, size, mtime, hash, me);
        index.insert(name, fi.clone());
        changed.push(fi);
    }

    // Anything we knew about but did not see is now deleted.
    let gone: Vec<String> = index
        .iter()
        .filter(|(n, f)| !f.deleted && !seen.contains(*n))
        .map(|(n, _)| n.clone())
        .collect();
    for name in gone {
        let old = index.get(&name).cloned();
        let kind = old.as_ref().map_or(Kind::File, |o| o.kind);
        let mut fi = new_version(old.as_ref(), name.clone(), kind, 0, 0, [0; 32], me);
        fi.deleted = true;
        index.insert(name, fi.clone());
        changed.push(fi);
    }

    Ok(changed)
}

fn new_version(
    old: Option<&FileInfo>,
    name: String,
    kind: Kind,
    size: u64,
    mtime: i64,
    hash: [u8; 32],
    me: u64,
) -> FileInfo {
    let version = old.map(|o| o.version.clone()).unwrap_or_default().bumped(me);
    FileInfo { name, kind, size, mtime, hash, deleted: false, version, modified_by: me }
}

pub fn load_index(path: &Path) -> Index {
    match std::fs::read(path) {
        Ok(bytes) => bincode::deserialize(&bytes).unwrap_or_else(|e| {
            log::error!("index {} unreadable ({e}); starting fresh", path.display());
            Index::new()
        }),
        Err(_) => Index::new(),
    }
}

pub fn save_index(path: &Path, index: &Index) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bincode::serialize(index)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
