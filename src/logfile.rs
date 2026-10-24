//! The background log file, kept small: when it reaches `MAX_BYTES` it is
//! moved to `minisync.log.1` (and that one to `.2`), so at most
//! `KEEP + 1` files of `MAX_BYTES` each exist, however long Mini Sync runs.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Old files kept next to the current one.
pub const KEEP: usize = 2;

pub struct RotatingLog {
    path: PathBuf,
    max: u64,
    keep: usize,
    file: Option<File>,
    size: u64,
}

impl RotatingLog {
    pub fn open(path: &Path) -> io::Result<RotatingLog> {
        let mut log = RotatingLog { path: path.to_path_buf(), max: MAX_BYTES, keep: KEEP, file: None, size: 0 };
        // Leftover from versions before rotation existed.
        let _ = std::fs::remove_file(path.with_extension("log.old"));
        log.reopen()?;
        if log.size >= log.max {
            log.rotate();
        }
        Ok(log)
    }

    fn old(&self, n: usize) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    }

    fn reopen(&mut self) -> io::Result<()> {
        let f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.size = f.metadata().map(|m| m.len()).unwrap_or(0);
        self.file = Some(f);
        Ok(())
    }

    /// minisync.log -> .1 -> .2 -> deleted. If the file can't be moved (e.g.
    /// another program has it open), it is emptied instead so it can't grow.
    fn rotate(&mut self) {
        self.file = None; // Windows can't rename a file that is still open
        let _ = std::fs::remove_file(self.old(self.keep));
        for n in (1..self.keep).rev() {
            let _ = std::fs::rename(self.old(n), self.old(n + 1));
        }
        if std::fs::rename(&self.path, self.old(1)).is_err() {
            let _ = OpenOptions::new().write(true).truncate(true).open(&self.path);
        }
        let _ = self.reopen();
    }
}

impl Write for RotatingLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.size + buf.len() as u64 > self.max && self.size > 0 {
            self.rotate();
        }
        match self.file.as_mut() {
            Some(f) => {
                let n = f.write(buf)?;
                self.size += n as u64;
                Ok(n)
            }
            None => Ok(buf.len()), // can't log right now; don't fail the program
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().map_or(Ok(()), |f| f.flush())
    }
}

/// True at most once per `every` for the same `key`. Used for warnings that
/// would otherwise repeat on every connection attempt.
pub fn throttle(key: &str, every: std::time::Duration) -> bool {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Instant;
    static LAST: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    let map = last.get_or_insert_with(HashMap::new);
    let now = Instant::now();
    match map.get(key) {
        Some(t) if now.duration_since(*t) < every => false,
        _ => {
            map.insert(key.to_string(), now);
            true
        }
    }
}

/// How often a repeated warning is written at most.
pub const REPEAT: std::time::Duration = std::time::Duration::from_secs(600);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_and_keeps_a_fixed_number_of_files() {
        let dir = std::env::temp_dir().join(format!("ms-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("minisync.log");
        let mut log = RotatingLog::open(&path).unwrap();
        log.max = 1000;
        let line = [b'x'; 99];
        for _ in 0..200 {
            log.write_all(&line).unwrap();
            log.write_all(b"\n").unwrap();
        }
        drop(log);
        let mut names: Vec<String> =
            std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["minisync.log", "minisync.log.1", "minisync.log.2"]);
        for n in &names {
            assert!(std::fs::metadata(dir.join(n)).unwrap().len() <= 1000, "{n} too big");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn throttle_once_per_window() {
        let every = std::time::Duration::from_secs(60);
        assert!(throttle("test-key", every));
        assert!(!throttle("test-key", every));
        assert!(throttle("other-key", every));
    }
}
