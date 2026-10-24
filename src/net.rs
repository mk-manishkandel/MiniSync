//! Encrypted, mutually authenticated connections between devices (Noise XX with
//! pinned static keys, standing in for Syncthing's TLS + device ID check),
//! LAN discovery by UDP broadcast, and the connection registry ("hub").

use crate::config::{Access, DeviceId, Home, Invite, Keypair, NOISE_PARAMS};
use crate::proto::{self, FileInfo, Message, Offer};
use crate::util;
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, oneshot};

pub const DISCOVERY_PORT: u16 = 21100;
const DISCOVERY_MAGIC: &[u8; 5] = b"MSYN1";
const NOISE_CHUNK: usize = 65000;
const MAX_REQUEST: u32 = 4 * 1024 * 1024;
const IDLE_TIMEOUT: Duration = Duration::from_secs(180);

pub enum FolderEvent {
    Scan,
    PeerConnected(DeviceId),
    PeerDisconnected(DeviceId),
    /// Files from a peer; the flag marks the end of a full list.
    RemoteIndex(DeviceId, Vec<FileInfo>, bool),
}

pub struct FolderHandle {
    pub root: PathBuf,
    pub events: mpsc::Sender<FolderEvent>,
}

/// Who a folder is synced with, and how. Kept in the hub so it can change
/// while running (the owner may change our access at any time).
#[derive(Debug, Clone)]
pub struct Rule {
    /// None = this device owns the folder.
    pub owner: Option<DeviceId>,
    /// For a joined folder: what the owner allows us.
    pub access: Access,
    /// For an owned folder: who has access.
    pub shares: HashMap<DeviceId, Access>,
    pub label: String,
}

impl Rule {
    /// May `peer` get this folder's files from us?
    pub fn can_send_to(&self, peer: &DeviceId) -> bool {
        match self.owner {
            None => self.shares.contains_key(peer),
            // Joined folders sync only with their owner, and only if we may write.
            Some(o) => o == *peer && self.access == Access::ReadWrite,
        }
    }

    /// Do we take changes from `peer`?
    pub fn accepts_from(&self, peer: &DeviceId) -> bool {
        match self.owner {
            None => self.shares.get(peer) == Some(&Access::ReadWrite),
            Some(o) => o == *peer,
        }
    }

    /// For a folder joined read-only: its owner (local edits are undone).
    pub fn read_only_owner(&self) -> Option<DeviceId> {
        self.owner.filter(|_| self.access == Access::ReadOnly)
    }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Vec<u8>, String>>>>>;

struct Conn {
    id: u64,
    initiator: DeviceId,
    tx: mpsc::Sender<Message>,
    pending: Pending,
}

pub struct Hub {
    pub me: DeviceId,
    private_key: Vec<u8>,
    pub device_name: String,
    listen_port: u16,
    peers: HashMap<DeviceId, (String, Vec<String>)>,
    pub folders: HashMap<String, FolderHandle>,
    rules: Mutex<HashMap<String, Rule>>,
    home_dir: PathBuf,
    /// Invitations waiting for an answer (shown in the tray).
    pub invites: AtomicUsize,
    conns: Mutex<HashMap<DeviceId, Conn>>,
    discovered: Mutex<HashMap<DeviceId, SocketAddr>>,
    dial_now: tokio::sync::Notify,
    next_id: AtomicU64,
}

impl Hub {
    pub fn new(
        key: Keypair,
        device_name: String,
        listen_port: u16,
        peers: HashMap<DeviceId, (String, Vec<String>)>,
        folders: HashMap<String, FolderHandle>,
        rules: HashMap<String, Rule>,
        home_dir: PathBuf,
        invites: usize,
    ) -> Hub {
        Hub {
            me: key.public,
            private_key: key.private,
            device_name,
            listen_port,
            peers,
            folders,
            rules: Mutex::new(rules),
            home_dir,
            invites: AtomicUsize::new(invites),
            conns: Mutex::new(HashMap::new()),
            discovered: Mutex::new(HashMap::new()),
            dial_now: tokio::sync::Notify::new(),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn peer_name(&self, id: &DeviceId) -> String {
        self.peers.get(id).map(|p| p.0.clone()).unwrap_or_else(|| format!("{id:?}"))
    }

    pub fn rule(&self, folder: &str) -> Option<Rule> {
        self.rules.lock().unwrap().get(folder).cloned()
    }

    /// The folders we own and share with `peer`.
    fn offers_for(&self, peer: &DeviceId) -> Vec<Offer> {
        let rules = self.rules.lock().unwrap();
        let mut offers: Vec<Offer> = rules
            .iter()
            .filter(|(_, r)| r.owner.is_none())
            .filter_map(|(id, r)| r.shares.get(peer).map(|a| Offer { folder: id.clone(), label: r.label.clone(), access: *a }))
            .collect();
        offers.sort_by(|a, b| a.folder.cmp(&b.folder));
        offers
    }

    /// Applies what `peer` says it shares with us: new invitations, changed
    /// access to folders we joined from it, and folders it stopped sharing.
    fn handle_offers(&self, peer: DeviceId, offers: &[Offer]) -> Result<()> {
        let home = Home { dir: self.home_dir.clone() };
        let mut cfg = home.load_config()?;
        let from = peer.to_string();
        let name = self.peer_name(&peer);
        let mut changed = false;
        let mut revoked = Vec::new();

        for f in cfg.folders.iter_mut().filter(|f| f.owner_id() == Some(peer)) {
            match offers.iter().find(|o| o.folder == f.id) {
                Some(o) if o.access != f.access => {
                    log::warn!("[{}] {name} changed your access to {}", f.id, o.access.label());
                    f.access = o.access;
                    if let Some(r) = self.rules.lock().unwrap().get_mut(&f.id) {
                        r.access = o.access;
                    }
                    changed = true;
                }
                Some(_) => {}
                None => revoked.push(f.id.clone()),
            }
        }
        for id in revoked {
            log::warn!("[{id}] {name} stopped sharing this folder with you; it no longer syncs (files are kept)");
            self.rules.lock().unwrap().remove(&id);
            home.remove_folder(&mut cfg, &id)?;
            changed = true;
        }

        for o in offers {
            if cfg.folders.iter().any(|f| f.id == o.folder) {
                continue; // already joined (or our own folder with the same ID)
            }
            match cfg.invites.iter_mut().find(|i| i.from == from && i.folder == o.folder) {
                Some(i) => {
                    if i.access != o.access || i.label != o.label {
                        i.access = o.access;
                        i.label = o.label.clone();
                        changed = true;
                    }
                }
                None => {
                    log::info!("{name} invites you to the folder {:?} ({}); accept it in the menu", o.label, o.access.label());
                    cfg.invites.push(Invite {
                        from: from.clone(),
                        folder: o.folder.clone(),
                        label: o.label.clone(),
                        access: o.access,
                        declined: false,
                    });
                    changed = true;
                }
            }
        }
        let before = cfg.invites.len();
        cfg.invites.retain(|i| i.from != from || offers.iter().any(|o| o.folder == i.folder));
        changed |= cfg.invites.len() != before;

        if changed {
            home.save_config(&cfg)?;
        }
        self.invites.store(cfg.pending_invites(), Ordering::Relaxed);
        Ok(())
    }

    pub fn connected(&self) -> Vec<DeviceId> {
        self.conns.lock().unwrap().keys().copied().collect()
    }

    pub async fn send(&self, peer: DeviceId, msg: Message) {
        let tx = self.conns.lock().unwrap().get(&peer).map(|c| c.tx.clone());
        if let Some(tx) = tx {
            let _ = tx.send(msg).await;
        }
    }

    /// Fetches a byte range of a file from a peer.
    pub async fn request(&self, peer: DeviceId, folder: &str, name: &str, offset: u64, size: u32) -> Result<Vec<u8>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (done_tx, done_rx) = oneshot::channel();
        let tx = {
            let conns = self.conns.lock().unwrap();
            let c = conns.get(&peer).ok_or_else(|| anyhow!("not connected"))?;
            c.pending.lock().unwrap().insert(id, done_tx);
            c.tx.clone()
        };
        tx.send(Message::Request { id, folder: folder.into(), name: name.into(), offset, size })
            .await
            .map_err(|_| anyhow!("connection closed"))?;
        match tokio::time::timeout(Duration::from_secs(60), done_rx).await {
            Ok(Ok(Ok(data))) => Ok(data),
            Ok(Ok(Err(e))) => bail!("peer: {e}"),
            Ok(Err(_)) => bail!("connection closed"),
            Err(_) => bail!("request timed out"),
        }
    }

    /// Registers a new connection. When both devices dial each other at the same
    /// time, both keep the connection started by the smaller device id.
    fn register(&self, peer: DeviceId, initiator: DeviceId, tx: mpsc::Sender<Message>) -> Option<(u64, Pending)> {
        let mut conns = self.conns.lock().unwrap();
        if let Some(existing) = conns.get(&peer) {
            let preferred = self.me.min(peer);
            if existing.initiator == preferred || initiator != preferred {
                return None;
            }
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let pending: Pending = Arc::default();
        conns.insert(peer, Conn { id, initiator, tx, pending: pending.clone() });
        Some((id, pending))
    }

    fn unregister(&self, peer: DeviceId, conn_id: u64) -> bool {
        let mut conns = self.conns.lock().unwrap();
        if conns.get(&peer).map_or(false, |c| c.id == conn_id) {
            conns.remove(&peer);
            true
        } else {
            false
        }
    }

    async fn notify_folders(&self, ev: impl Fn() -> FolderEvent) {
        for f in self.folders.values() {
            let _ = f.events.send(ev()).await;
        }
    }
}

// ---------- framing + Noise ----------

async fn write_frame(w: &mut OwnedWriteHalf, data: &[u8]) -> Result<()> {
    w.write_all(&(data.len() as u16).to_be_bytes()).await?;
    w.write_all(data).await?;
    Ok(())
}

async fn read_frame(r: &mut OwnedReadHalf) -> Result<Vec<u8>> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len).await?;
    let mut buf = vec![0u8; u16::from_be_bytes(len) as usize];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn handshake(
    hub: &Hub,
    r: &mut OwnedReadHalf,
    w: &mut OwnedWriteHalf,
    initiator: bool,
) -> Result<(DeviceId, snow::TransportState)> {
    let builder = snow::Builder::new(NOISE_PARAMS.parse()?).local_private_key(&hub.private_key);
    let mut hs = if initiator { builder.build_initiator()? } else { builder.build_responder()? };
    let mut buf = vec![0u8; 65535];
    let mut scratch = vec![0u8; 65535];
    // XX pattern: -> e, <- e ee s es, -> s se
    for step in 0..3 {
        let we_write = (step % 2 == 0) == initiator;
        if we_write {
            let n = hs.write_message(&[], &mut buf)?;
            write_frame(w, &buf[..n]).await?;
        } else {
            let msg = read_frame(r).await?;
            hs.read_message(&msg, &mut scratch)?;
        }
    }
    let remote = hs.get_remote_static().ok_or_else(|| anyhow!("no remote key"))?;
    let remote = DeviceId(remote.try_into().map_err(|_| anyhow!("bad remote key"))?);
    Ok((remote, hs.into_transport_mode()?))
}

async fn send_msg(w: &mut OwnedWriteHalf, noise: &Mutex<snow::TransportState>, msg: &Message) -> Result<()> {
    let body = proto::encode(msg)?;
    let mut plain = Vec::with_capacity(body.len() + 4);
    plain.extend_from_slice(&(body.len() as u32).to_be_bytes());
    plain.extend_from_slice(&body);
    let mut frames = Vec::new();
    {
        let mut noise = noise.lock().unwrap();
        for chunk in plain.chunks(NOISE_CHUNK) {
            let mut out = vec![0u8; chunk.len() + 16];
            let n = noise.write_message(chunk, &mut out)?;
            out.truncate(n);
            frames.push(out);
        }
    }
    for f in frames {
        write_frame(w, &f).await?;
    }
    Ok(())
}

async fn recv_msg(r: &mut OwnedReadHalf, noise: &Mutex<snow::TransportState>) -> Result<Message> {
    let mut plain = Vec::new();
    let mut out = vec![0u8; 65535];
    loop {
        let frame = read_frame(r).await?;
        let n = noise.lock().unwrap().read_message(&frame, &mut out)?;
        plain.extend_from_slice(&out[..n]);
        if plain.len() >= 4 {
            let len = u32::from_be_bytes(plain[..4].try_into().unwrap()) as usize;
            if len > proto::MAX_MESSAGE {
                bail!("message too large ({len} bytes)");
            }
            if plain.len() >= 4 + len {
                return proto::decode(&plain[4..4 + len]);
            }
        }
    }
}

// ---------- connection lifecycle ----------

pub async fn run_conn(hub: Arc<Hub>, stream: TcpStream, initiator: bool) {
    let addr = stream.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let _ = stream.set_nodelay(true);
    let (mut r, mut w) = stream.into_split();
    let (peer, noise) = match tokio::time::timeout(Duration::from_secs(10), handshake(&hub, &mut r, &mut w, initiator)).await {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => {
            log::debug!("handshake with {addr} failed: {e}");
            return;
        }
        Err(_) => {
            log::debug!("handshake with {addr} timed out");
            return;
        }
    };
    if !hub.peers.contains_key(&peer) {
        if crate::logfile::throttle(&format!("unknown-{peer}"), crate::logfile::REPEAT) {
            log::warn!("unknown device {peer} at {addr} tried to connect; add it with `minisync add-peer` if it is yours");
        }
        return;
    }
    let initiator_id = if initiator { hub.me } else { peer };
    let (tx, mut rx) = mpsc::channel::<Message>(256);
    let Some((conn_id, pending)) = hub.register(peer, initiator_id, tx.clone()) else {
        log::debug!("duplicate connection to {}, dropping", hub.peer_name(&peer));
        return;
    };
    let name = hub.peer_name(&peer);
    log::info!("connected to {name} ({addr})");

    let noise = Arc::new(Mutex::new(noise));
    let writer_noise = noise.clone();
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Err(e) = send_msg(&mut w, &writer_noise, &msg).await {
                log::debug!("write: {e}");
                break;
            }
        }
        let _ = w.shutdown().await;
    });
    let pinger_tx = tx.clone();
    let pinger = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            if pinger_tx.send(Message::Ping).await.is_err() {
                break;
            }
        }
    });

    let _ = tx
        .send(Message::Hello {
            protocol: proto::PROTOCOL,
            device_name: hub.device_name.clone(),
            offers: hub.offers_for(&peer),
        })
        .await;
    drop(tx);
    hub.notify_folders(|| FolderEvent::PeerConnected(peer)).await;

    let mut got_hello = false;
    loop {
        let msg = match tokio::time::timeout(IDLE_TIMEOUT, recv_msg(&mut r, &noise)).await {
            Ok(Ok(m)) => m,
            Ok(Err(e)) if !got_hello => {
                if crate::logfile::throttle(&format!("talk-{peer}"), crate::logfile::REPEAT) {
                    log::warn!("could not talk to {name} ({e}); make sure both devices run the latest Mini Sync");
                }
                break;
            }
            Ok(Err(e)) => {
                log::debug!("read from {name}: {e}");
                break;
            }
            Err(_) => {
                log::info!("{name} timed out");
                break;
            }
        };
        match msg {
            Message::Hello { protocol, device_name, offers } => {
                if protocol != proto::PROTOCOL {
                    if crate::logfile::throttle(&format!("version-{peer}"), crate::logfile::REPEAT) {
                        log::warn!("{name} runs a different Mini Sync version; update both devices");
                    }
                    break;
                }
                got_hello = true;
                let list: Vec<String> = offers.iter().map(|o| format!("{} ({})", o.label, o.access.label())).collect();
                log::info!("{name} calls itself {device_name:?}, shares with you: {list:?}");
                if let Err(e) = hub.handle_offers(peer, &offers) {
                    log::error!("saving folders shared by {name}: {e:#}");
                }
            }
            Message::Index { folder, files, complete } => {
                match (hub.folders.get(&folder), hub.rule(&folder)) {
                    (Some(f), Some(rule)) if rule.accepts_from(&peer) => {
                        let _ = f.events.send(FolderEvent::RemoteIndex(peer, files, complete)).await;
                    }
                    _ => log::debug!("ignoring index for {folder:?} from {name} (not allowed)"),
                }
            }
            Message::Request { id, folder, name: file, offset, size } => {
                let hub = hub.clone();
                tokio::spawn(async move {
                    let allowed = hub.rule(&folder).is_some_and(|r| r.can_send_to(&peer));
                    let data = if allowed {
                        serve_request(&hub, &folder, &file, offset, size).await
                    } else {
                        if crate::logfile::throttle(&format!("refused-{peer}-{folder}"), crate::logfile::REPEAT) {
                            log::warn!("refused {} a file from {folder:?}: no access", hub.peer_name(&peer));
                        }
                        Err("no access to this folder".into())
                    };
                    hub.send(peer, Message::Response { id, data }).await;
                });
            }
            Message::Response { id, data } => {
                if let Some(done) = pending.lock().unwrap().remove(&id) {
                    let _ = done.send(data);
                }
            }
            Message::Ping => {}
        }
    }

    pinger.abort();
    if hub.unregister(peer, conn_id) {
        log::info!("disconnected from {name}");
        pending.lock().unwrap().clear();
        hub.notify_folders(|| FolderEvent::PeerDisconnected(peer)).await;
    }
    writer.abort();
}

async fn serve_request(hub: &Hub, folder: &str, name: &str, offset: u64, size: u32) -> Result<Vec<u8>, String> {
    let root = hub.folders.get(folder).ok_or("no such folder")?.root.clone();
    let path = util::safe_join(&root, name).ok_or("invalid name")?;
    let size = size.min(MAX_REQUEST) as usize;
    tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        let mut f = std::fs::File::open(&path)?;
        f.seek(SeekFrom::Start(offset))?;
        let mut buf = Vec::with_capacity(size);
        f.take(size as u64).read_to_end(&mut buf)?;
        Ok(buf)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

// ---------- listening, dialing, discovery ----------

pub async fn listen(hub: Arc<Hub>, listener: TcpListener) -> Result<()> {
    log::info!("listening on port {}", hub.listen_port);
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(run_conn(hub.clone(), stream, false));
    }
}

pub async fn dial_loop(hub: Arc<Hub>) {
    loop {
        let connected = hub.connected();
        for (id, (_, addrs)) in &hub.peers {
            if connected.contains(id) {
                continue;
            }
            let mut targets: Vec<String> = addrs.clone();
            if let Some(a) = hub.discovered.lock().unwrap().get(id) {
                targets.push(a.to_string());
            }
            for t in targets {
                if let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&t)).await {
                    tokio::spawn(run_conn(hub.clone(), stream, true));
                    break;
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(15)) => {}
            _ = hub.dial_now.notified() => {}
        }
    }
}

/// Announces "device X listens on port P" to the LAN every 20s and records
/// announcements from configured peers. Unauthenticated on purpose: a fake
/// announcement can only cause a failed handshake.
pub async fn discovery(hub: Arc<Hub>) {
    let recv = match UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)).await {
        Ok(s) => Some(s),
        Err(e) => {
            log::warn!("LAN discovery listener unavailable ({e}); only fixed addresses will be used");
            None
        }
    };
    if let Some(sock) = recv {
        let hub = hub.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 64];
            while let Ok((n, from)) = sock.recv_from(&mut buf).await {
                if n != 39 || &buf[..5] != DISCOVERY_MAGIC {
                    continue;
                }
                let id = DeviceId(buf[5..37].try_into().unwrap());
                if id == hub.me || !hub.peers.contains_key(&id) {
                    continue;
                }
                let port = u16::from_be_bytes([buf[37], buf[38]]);
                let addr = SocketAddr::new(from.ip(), port);
                let old = hub.discovered.lock().unwrap().insert(id, addr);
                if old != Some(addr) {
                    log::info!("found {} at {addr}", hub.peer_name(&id));
                    hub.dial_now.notify_one();
                }
            }
        });
    }

    let send = match UdpSocket::bind(("0.0.0.0", 0)).await {
        Ok(s) => s,
        Err(e) => {
            log::warn!("LAN discovery announcer unavailable: {e}");
            return;
        }
    };
    let _ = send.set_broadcast(true);
    let mut packet = Vec::with_capacity(39);
    packet.extend_from_slice(DISCOVERY_MAGIC);
    packet.extend_from_slice(&hub.me.0);
    packet.extend_from_slice(&hub.listen_port.to_be_bytes());
    loop {
        for target in broadcast_targets() {
            if let Err(e) = send.send_to(&packet, (target, DISCOVERY_PORT)).await {
                log::debug!("discovery broadcast to {target}: {e}");
            }
        }
        tokio::time::sleep(Duration::from_secs(20)).await;
    }
}

/// The generic broadcast address only leaves through one interface on some
/// systems (often a VPN), so also target each interface's own broadcast address.
fn broadcast_targets() -> Vec<Ipv4Addr> {
    let mut targets = vec![Ipv4Addr::BROADCAST, Ipv4Addr::LOCALHOST];
    for iface in if_addrs::get_if_addrs().unwrap_or_default() {
        if let if_addrs::IfAddr::V4(v4) = iface.addr {
            if v4.ip.is_loopback() || v4.netmask == Ipv4Addr::BROADCAST {
                continue;
            }
            let bcast = v4
                .broadcast
                .unwrap_or_else(|| Ipv4Addr::from(u32::from(v4.ip) | !u32::from(v4.netmask)));
            if !targets.contains(&bcast) {
                targets.push(bcast);
            }
        }
    }
    targets
}
