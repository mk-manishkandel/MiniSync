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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub device_name: String,
    #[serde(default = "default_port")]
    pub listen_port: u16,
    #[serde(default)]
    pub folders: Vec<FolderConfig>,
    #[serde(default)]
    pub peers: Vec<PeerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderConfig {
    /// Must be the same on every device sharing this folder.
    pub id: String,
    pub path: PathBuf,
    /// Simple patterns matched against file/dir names: exact name, "*.ext", "prefix*".
    #[serde(default = "default_ignores")]
    pub ignore: Vec<String>,
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
        self.save_config(cfg)?;
        Ok(peer)
    }

    pub fn load_config(&self) -> Result<Config> {
        let path = self.config_path();
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {} (run `minisync init` first)", path.display()))?;
        let cfg: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        for p in &cfg.peers {
            DeviceId::parse(&p.id).with_context(|| format!("peer {:?}", p.name))?;
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
