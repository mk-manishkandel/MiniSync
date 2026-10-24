use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Everything minisync keeps inside a synced folder starts with this prefix
/// and is never synced itself.
pub const INTERNAL_PREFIX: &str = ".minisync";
/// Marker file proving the folder is really there (protects against an unmounted
/// drive looking like "everything was deleted").
pub const MARKER: &str = ".minisync-folder";
pub const TMP_DIR: &str = ".minisync-tmp";
pub const TRASH_DIR: &str = ".minisync-trash";

pub fn is_ignored(patterns: &[String], file_name: &str) -> bool {
    file_name.starts_with(INTERNAL_PREFIX) || patterns.iter().any(|p| wildcard(p, file_name))
}

/// Minimal glob: '*' matches any run of characters, case-insensitive.
fn wildcard(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut ni) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == n[ni] || p[pi] == '?') {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Turns a path under `root` into the '/'-separated name used on the wire.
pub fn rel_name(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?.to_string()),
            _ => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Resolves a name received from a peer to a local path, refusing anything
/// that could escape the folder or that this OS cannot represent.
pub fn safe_join(root: &Path, name: &str) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    for part in name.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.starts_with(INTERNAL_PREFIX) {
            return None;
        }
        if part.contains('\\') || part.contains('\0') {
            return None;
        }
        if cfg!(windows)
            && (part.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (c as u32) < 32)
                || part.ends_with(' ')
                || part.ends_with('.'))
        {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

pub fn mtime_secs(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn hash_file(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matching() {
        assert!(wildcard("*.tmp", "a.TMP"));
        assert!(wildcard("~$*", "~$report.docx"));
        assert!(wildcard(".DS_Store", ".ds_store"));
        assert!(!wildcard("*.tmp", "a.tmp.txt"));
    }

    #[test]
    fn safe_join_rejects_escapes() {
        let root = Path::new("root");
        assert!(safe_join(root, "../x").is_none());
        assert!(safe_join(root, "a//b").is_none());
        assert!(safe_join(root, ".minisync-tmp/x").is_none());
        assert!(safe_join(root, "a/b.txt").is_some());
    }
}
