use anyhow::{bail, Context, Result};
use data_encoding::BASE32_NOPAD;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// A device is identified by its 32-byte Noise static public key,
/// the same idea as Syncthing's certificate-fingerprint device ID.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeviceId(pub [u8; 32]);

impl DeviceId {
    /// Short numeric form used as the key in version vectors.
    pub fn short(&self) -> u64 {
        u64::from_be_bytes(self.0[..8].try_into().unwrap())
    }

    pub fn parse(s: &str) -> Result<DeviceId> {
        let clean: String = s
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_uppercase())
            .collect();
        let bytes = BASE32_NOPAD
            .decode(clean.as_bytes())
            .with_context(|| format!("invalid device id {s:?}"))?;
        if bytes.len() != 32 {
            bail!("invalid device id {s:?}: wrong length");
        }
        Ok(DeviceId(bytes.try_into().unwrap()))
    }
}

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let enc = BASE32_NOPAD.encode(&self.0);
        let groups: Vec<&str> = enc
            .as_bytes()
            .chunks(7)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        write!(f, "{}", groups.join("-"))
    }
}

impl std::fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", &self.to_string()[..7])
    }
}

/// Settings layout version. 0 = before folder sharing rules existed.
pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub version: u32,
    pub device_name: String,
    #[serde(default = "default_port")]
    pub listen_port: u16,
    #[serde(default)]
    pub folders: Vec<FolderConfig>,
    #[serde(default)]
    pub peers: Vec<PeerConfig>,
    /// Folders other devices have offered to share with this one.
    #[serde(default)]
    pub invites: Vec<Invite>,
}

/// What a device may do with a shared folder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Access {
    #[default]
    ReadWrite,
    ReadOnly,
}

impl Access {
    pub fn is_read_write(&self) -> bool {
        *self == Access::ReadWrite
    }

    pub fn label(self) -> &'static str {
        match self {
            Access::ReadWrite => "read & write",
            Access::ReadOnly => "read only",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderConfig {
    /// Must be the same on every device sharing this folder.
    pub id: String,
    pub path: PathBuf,
    /// Simple patterns matched against file/dir names: exact name, "*.ext", "prefix*".
    #[serde(default = "default_ignores")]
    pub ignore: Vec<String>,
    /// The device that shared this folder with us. None = this device owns it.
    /// Only the owner decides who gets the folder and with what access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// For a folder we joined: what the owner allows this device to do.
    #[serde(default, skip_serializing_if = "Access::is_read_write")]
    pub access: Access,
    /// For a folder we own: the devices it is shared with.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_with: Vec<Share>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Share {
    pub device: String,
    pub access: Access,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invite {
    /// Device ID of the folder's owner.
    pub from: String,
    pub folder: String,
    /// The folder's name on the owner's device.
    pub label: String,
    pub access: Access,
    #[serde(default)]
    pub declined: bool,
}

impl FolderConfig {
    /// A new folder owned by this device, shared with nobody yet.
    pub fn new_own(path: PathBuf) -> FolderConfig {
        let id = new_folder_id(&path);
        FolderConfig { id, path, ignore: default_ignores(), owner: None, access: Access::ReadWrite, shared_with: vec![] }
    }

    pub fn owner_id(&self) -> Option<DeviceId> {
        self.owner.as_deref().and_then(|s| DeviceId::parse(s).ok())
    }

    pub fn is_mine(&self) -> bool {
        self.owner.is_none()
    }

    pub fn share_for(&self, dev: &DeviceId) -> Option<Access> {
        self.shared_with
            .iter()
            .find(|s| DeviceId::parse(&s.device).ok().as_ref() == Some(dev))
            .map(|s| s.access)
    }

    /// Gives `dev` this access (None = no access).
    pub fn set_share(&mut self, dev: &DeviceId, access: Option<Access>) {
        self.shared_with.retain(|s| DeviceId::parse(&s.device).ok().as_ref() != Some(dev));
        if let Some(access) = access {
            self.shared_with.push(Share { device: dev.to_string(), access });
        }
    }

    /// Display name: the folder's own name.
    pub fn label(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

/// "My Photos" -> "my-photos-K3J9QX": readable, and unique so folders shared
/// from different devices never collide.
pub fn new_folder_id(path: &Path) -> String {
    use std::hash::{BuildHasher, Hasher};
    let slug: String = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write(path.to_string_lossy().as_bytes());
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    let tag = &BASE32_NOPAD.encode(&h.finish().to_be_bytes())[..6];
    if slug.is_empty() {
        format!("folder-{tag}")
    } else {
        format!("{slug}-{tag}")
    }
}

impl Config {
    pub fn new(device_name: String, listen_port: u16) -> Config {
        Config { version: CONFIG_VERSION, device_name, listen_port, folders: vec![], peers: vec![], invites: vec![] }
    }

    /// Adds a trusted device, or replaces the one with the same name or ID.
    /// If an existing device got a new ID, its folder permissions move along.
    /// Returns true if an existing device was replaced.
    pub fn upsert_peer(&mut self, peer: PeerConfig) -> bool {
        let new_id = DeviceId::parse(&peer.id).ok();
        let old: Vec<String> = self
            .peers
            .iter()
            .filter(|p| p.name == peer.name || DeviceId::parse(&p.id).ok() == new_id)
            .map(|p| p.id.clone())
            .collect();
        self.peers.retain(|p| !old.contains(&p.id));
        let new = peer.id.clone();
        for f in &mut self.folders {
            for s in &mut f.shared_with {
                if old.contains(&s.device) {
                    s.device = new.clone();
                }
            }
            if f.owner.as_ref().is_some_and(|o| old.contains(o)) {
                f.owner = Some(new.clone());
            }
        }
        self.peers.push(peer);
        !old.is_empty()
    }

    pub fn peer_name(&self, id: &str) -> String {
        let parsed = DeviceId::parse(id).ok();
        self.peers
            .iter()
            .find(|p| DeviceId::parse(&p.id).ok() == parsed)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| format!("unknown device {}", &id[..id.len().min(7)]))
    }

    pub fn pending_invites(&self) -> usize {
        self.invites.iter().filter(|i| !i.declined).count()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConfig {
    pub name: String,
    pub id: String,
    /// Optional fixed "ip:port" addresses. Devices on the same LAN are found automatically.
    #[serde(default)]
    pub addresses: Vec<String>,
}

fn default_port() -> u16 {
    22100
}

pub fn default_ignores() -> Vec<String> {
    [".DS_Store", "Thumbs.db", "desktop.ini", "*.tmp", "~$*", ".~lock*"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

pub struct Home {
    pub dir: PathBuf,
}

impl Home {
    pub fn new(dir: Option<PathBuf>) -> Result<Home> {
        let dir = match dir {
            Some(d) => d,
            None => dirs::config_dir()
                .context("cannot find a config directory")?
                .join("minisync"),
        };
        Ok(Home { dir })
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.join("config.toml")
    }

    /// Folder progress, written by the background process for the menu.
    pub fn status_path(&self) -> PathBuf {
        self.dir.join("status.json")
    }

    pub fn key_path(&self) -> PathBuf {
        self.dir.join("key.bin")
    }

    pub fn index_path(&self, folder_id: &str) -> PathBuf {
        let safe: String = folder_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        self.dir.join(format!("index-{safe}.bin"))
    }

    /// Stops syncing a folder: drops it from the config and forgets its index,
    /// so adding it again later starts clean. The files themselves are untouched.
    pub fn remove_folder(&self, cfg: &mut Config, id: &str) -> Result<FolderConfig> {
        let pos = cfg
            .folders
            .iter()
            .position(|f| f.id == id)
            .with_context(|| format!("no folder with ID {id:?}"))?;
        let folder = cfg.folders.remove(pos);
        self.save_config(cfg)?;
        let _ = std::fs::remove_file(self.index_path(&folder.id));
        let _ = std::fs::remove_file(folder.path.join(crate::util::MARKER));
        let _ = std::fs::remove_dir_all(folder.path.join(crate::util::TMP_DIR));
        Ok(folder)
    }

    pub fn remove_peer(&self, cfg: &mut Config, name_or_id: &str) -> Result<PeerConfig> {
        let as_id = DeviceId::parse(name_or_id).ok();
        let pos = cfg
            .peers
            .iter()
            .position(|p| p.name == name_or_id || (as_id.is_some() && DeviceId::parse(&p.id).ok() == as_id))
            .with_context(|| format!("no device named {name_or_id:?}"))?;
        let peer = cfg.peers.remove(pos);
        // Forget everything about it: its access to our folders, its
        // invitations, and the folders we joined from it (files are kept).
        for f in &mut cfg.folders {
            f.shared_with.retain(|s| s.device != peer.id);
        }
        cfg.invites.retain(|i| i.from != peer.id);
        let joined: Vec<String> =
            cfg.folders.iter().filter(|f| f.owner.as_ref() == Some(&peer.id)).map(|f| f.id.clone()).collect();
        for id in joined {
            self.remove_folder(cfg, &id)?;
        }
        self.save_config(cfg)?;
        Ok(peer)
    }

    pub fn load_config(&self) -> Result<Config> {
        let path = self.config_path();
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {} (run `minisync init` first)", path.display()))?;
        let mut cfg: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        for p in &cfg.peers {
            DeviceId::parse(&p.id).with_context(|| format!("peer {:?}", p.name))?;
        }
        if cfg.version < CONFIG_VERSION {
            // Before sharing rules existed every folder went to every device
            // with full access. Keep it that way for existing setups.
            for f in cfg.folders.iter_mut().filter(|f| f.owner.is_none() && f.shared_with.is_empty()) {
                f.shared_with =
                    cfg.peers.iter().map(|p| Share { device: p.id.clone(), access: Access::ReadWrite }).collect();
            }
            cfg.version = CONFIG_VERSION;
            self.save_config(&cfg)?;
        }
        // Versions up to 0.3.5 saved dragged-in paths with their quotes.
        let mut repaired = false;
        for f in &mut cfg.folders {
            let raw = f.path.to_string_lossy().to_string();
            let clean = crate::util::strip_quotes(&raw);
            if clean != raw {
                f.path = PathBuf::from(clean);
                repaired = true;
            }
        }
        if repaired {
            self.save_config(&cfg)?;
        }
        Ok(cfg)
    }

    pub fn save_config(&self, cfg: &Config) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(self.config_path(), toml::to_string_pretty(cfg)?)?;
        Ok(())
    }

    /// Returns (private, public) key, generating it on first use.
    pub fn load_or_create_key(&self) -> Result<Keypair> {
        let path = self.key_path();
        if path.exists() {
            let bytes = std::fs::read(&path)?;
            if bytes.len() != 64 {
                bail!("{} is corrupt", path.display());
            }
            return Ok(Keypair {
                private: bytes[..32].to_vec(),
                public: DeviceId(bytes[32..].try_into().unwrap()),
            });
        }
        let builder = snow::Builder::new(NOISE_PARAMS.parse()?);
        let kp = builder.generate_keypair()?;
        std::fs::create_dir_all(&self.dir)?;
        let mut bytes = kp.private.clone();
        bytes.extend_from_slice(&kp.public);
        write_private(&path, &bytes)?;
        Ok(Keypair {
            private: kp.private,
            public: DeviceId(kp.public.try_into().unwrap()),
        })
    }
}

impl Home {
    /// Replaces this device's key with a new one. The old device ID stops
    /// working; other devices must be given the new ID.
    pub fn rotate_key(&self) -> Result<Keypair> {
        let path = self.key_path();
        if path.exists() {
            // Overwrite before deleting so the old private key isn't left on disk.
            let _ = std::fs::write(&path, [0u8; 64]);
            std::fs::remove_file(&path)?;
        }
        self.load_or_create_key()
    }
}

pub struct Keypair {
    pub private: Vec<u8>,
    pub public: DeviceId,
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}
