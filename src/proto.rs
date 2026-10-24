//! Wire messages and file metadata. A heavily reduced version of Syncthing's
//! Block Exchange Protocol: Hello, Index, Request, Response, Ping.

use crate::config::Access;
use serde::{Deserialize, Serialize};

/// Version vector: one counter per device that has modified the file.
/// Lets us tell "newer" apart from "edited on two devices at once" (conflict).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vector(pub Vec<(u64, u64)>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Equal,
    Greater,
    Lesser,
    Concurrent,
}

impl Vector {
    fn get(&self, id: u64) -> u64 {
        self.0.iter().find(|(d, _)| *d == id).map(|(_, c)| *c).unwrap_or(0)
    }

    /// Returns a copy with this device's counter bumped past every other counter.
    pub fn bumped(&self, me: u64) -> Vector {
        let next = self.0.iter().map(|(_, c)| *c).max().unwrap_or(0) + 1;
        let mut v = self.clone();
        match v.0.iter_mut().find(|(d, _)| *d == me) {
            Some(e) => e.1 = next,
            None => v.0.push((me, next)),
        }
        v.0.sort();
        v
    }

    pub fn merged(&self, other: &Vector) -> Vector {
        let mut v = self.clone();
        for &(d, c) in &other.0 {
            match v.0.iter_mut().find(|(x, _)| *x == d) {
                Some(e) => e.1 = e.1.max(c),
                None => v.0.push((d, c)),
            }
        }
        v.0.sort();
        v
    }

    pub fn compare(&self, other: &Vector) -> Order {
        let mut greater = false;
        let mut lesser = false;
        let ids = self.0.iter().chain(other.0.iter()).map(|(d, _)| *d);
        for id in ids {
            let (a, b) = (self.get(id), other.get(id));
            if a > b {
                greater = true;
            }
            if a < b {
                lesser = true;
            }
        }
        match (greater, lesser) {
            (false, false) => Order::Equal,
            (true, false) => Order::Greater,
            (false, true) => Order::Lesser,
            (true, true) => Order::Concurrent,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    File,
    Dir,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    /// Path relative to the folder root, always with '/' separators.
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    /// Modification time, whole seconds since the Unix epoch.
    pub mtime: i64,
    /// SHA-256 of the whole file (zero for dirs and deleted entries).
    pub hash: [u8; 32],
    pub deleted: bool,
    pub version: Vector,
    /// Short id of the device that made the last change.
    pub modified_by: u64,
}

impl FileInfo {
    pub fn same_content(&self, other: &FileInfo) -> bool {
        self.kind == other.kind
            && self.deleted == other.deleted
            && (self.deleted
                || self.kind == Kind::Dir
                || (self.size == other.size && self.hash == other.hash))
    }

    /// Decides which side of a conflict keeps the original name. Both devices
    /// evaluate this identically, so they agree on the winner without talking.
    pub fn wins_conflict(&self, other: &FileInfo) -> bool {
        if self.deleted != other.deleted {
            return !self.deleted; // an edit beats a delete
        }
        if self.mtime != other.mtime {
            return self.mtime > other.mtime;
        }
        self.modified_by > other.modified_by
    }
}

/// Bumped whenever messages change in a way older versions can't read.
pub const PROTOCOL: u32 = 2;

/// A folder the sender owns and shares with the receiver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Offer {
    pub folder: String,
    pub label: String,
    pub access: Access,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Message {
    /// First message on every connection. `offers` lists the folders the
    /// sender owns and shares with the receiver (the owner's word is final).
    Hello { protocol: u32, device_name: String, offers: Vec<Offer> },
    /// Full or partial list of files; the receiver merges it either way.
    /// `complete` marks the last batch of a full list.
    Index { folder: String, files: Vec<FileInfo>, complete: bool },
    Request { id: u64, folder: String, name: String, offset: u64, size: u32 },
    Response { id: u64, data: Result<Vec<u8>, String> },
    Ping,
}

pub const MAX_MESSAGE: usize = 32 * 1024 * 1024;

pub fn encode(msg: &Message) -> anyhow::Result<Vec<u8>> {
    Ok(bincode::serialize(msg)?)
}

pub fn decode(bytes: &[u8]) -> anyhow::Result<Message> {
    Ok(bincode::deserialize(bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_ordering() {
        let a = Vector::default().bumped(1);
        let b = a.bumped(2);
        assert_eq!(b.compare(&a), Order::Greater);
        assert_eq!(a.compare(&b), Order::Lesser);
        let c = a.bumped(3);
        assert_eq!(b.compare(&c), Order::Concurrent);
        let m = b.merged(&c).bumped(1);
        assert_eq!(m.compare(&b), Order::Greater);
        assert_eq!(m.compare(&c), Order::Greater);
        assert_eq!(m.compare(&m.clone()), Order::Equal);
    }
}
