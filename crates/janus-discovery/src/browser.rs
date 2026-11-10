use crate::{
    Cache, Candidate, DESTINATION, DESTINATION_V6, MAX_CANDIDATES, MAX_PACKET,
    parse_hisense_broadcast, query_with_resolution, socket, socket_v6,
};
use anyhow::{Result, bail};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    io::ErrorKind,
    net::{IpAddr, Ipv4Addr, SocketAddrV4},
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    runtime::Handle,
    sync::watch,
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BrowseFailure {
    PermissionDenied,
    AddressInUse,
    NetworkUnavailable,
}

impl BrowseFailure {
    pub fn message(self) -> &'static str {
        match self {
            Self::PermissionDenied => {
                "Local network discovery is not permitted. Check application network permissions."
            }
            Self::AddressInUse => "Local discovery could not share the network discovery port.",
            Self::NetworkUnavailable => {
                "Local network discovery is unavailable. Check your network connection."
            }
        }
    }
    fn from_error(error: &anyhow::Error) -> Self {
        match error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind)
        {
            Some(ErrorKind::PermissionDenied) => Self::PermissionDenied,
            Some(ErrorKind::AddrInUse) => Self::AddressInUse,
            _ => Self::NetworkUnavailable,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BrowsePhase {
    Idle,
    Searching,
    Failed(BrowseFailure),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiscoverySnapshot {
    pub phase: BrowsePhase,
    pub devices: Vec<Candidate>,
}

impl Default for DiscoverySnapshot {
    fn default() -> Self {
        Self {
            phase: BrowsePhase::Searching,
            devices: Vec::new(),
        }
    }
}

#[derive(Clone, Copy)]
struct Request {
    enabled: bool,
    revision: u64,
}

/// One latest-value channel feeds the native UI: no unbounded update/event queue.
/// Dropping the browser cancels its task and releases the multicast socket.
pub struct Browser {
    state: watch::Receiver<DiscoverySnapshot>,
    request: watch::Sender<Request>,
    task: JoinHandle<()>,
}

impl Browser {
    pub fn start(runtime: &Handle) -> Self {
        let (updates, state) = watch::channel(DiscoverySnapshot::default());
        let (request, mut requests) = watch::channel(Request {
            enabled: true,
            revision: 0,
        });
        let task = runtime.spawn(async move {
            loop {
                let request = *requests.borrow_and_update();
                if !request.enabled {
                    updates.send_replace(DiscoverySnapshot {
                        phase: BrowsePhase::Idle,
                        devices: Vec::new(),
                    });
                    if requests.changed().await.is_err() {
                        break;
                    }
                    continue;
                }
                updates.send_replace(DiscoverySnapshot::default());
                tokio::select! {
                    result = scan(&updates) => {
                        if let Err(error) = result {
                            updates.send_replace(DiscoverySnapshot { phase: BrowsePhase::Failed(BrowseFailure::from_error(&error)), devices: Vec::new() });
                        }
                        if requests.changed().await.is_err() { break; }
                    }
                    result = requests.changed() => { if result.is_err() { break; } }
                    _ = updates.closed() => break,
                }
            }
        });
        Self {
            state,
            request,
            task,
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<DiscoverySnapshot> {
        self.state.clone()
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.request.send_if_modified(|request| {
            let changed = request.enabled != enabled;
            request.enabled = enabled;
            changed
        });
    }

    pub fn retry(&self) {
        self.request.send_modify(|request| {
            request.revision = request.revision.wrapping_add(1);
        });
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Bounded CLI inspection uses the same scanner/cache as the native UI.
pub async fn browse(duration: Duration) -> Result<Vec<Candidate>> {
    if duration.is_zero() || duration > Duration::from_secs(60) {
        bail!("browse duration must be within 0–60 seconds");
    }
    let (updates, state) = watch::channel(DiscoverySnapshot::default());
    tokio::select! {
        result = scan(&updates) => { result?; unreachable!("scanner only ends on error"); }
        _ = tokio::time::sleep(duration) => Ok(state.borrow().devices.clone()),
    }
}

const HISENSE_UDP_PORT: u16 = 48689;
const HISENSE_TTL_MS: u64 = 10_000;

fn hisense_socket() -> Option<UdpSocket> {
    let std_sock = std::net::UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    std_sock.set_broadcast(true).ok()?;
    std_sock.set_nonblocking(true).ok()?;
    UdpSocket::from_std(std_sock).ok()
}

async fn scan(updates: &watch::Sender<DiscoverySnapshot>) -> Result<()> {
    let socket = socket()?;
    // Best effort: IPv6 adds announcements; its absence keeps IPv4 behaviour.
    let socket6 = socket_v6().ok();
    // EShare boards also answer on their own UDP discovery port.
    let hisense_udp = hisense_socket();
    let started = Instant::now();
    let mut refresh = tokio::time::interval(Duration::from_secs(1));
    refresh.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut expiry = tokio::time::interval(Duration::from_millis(200));
    expiry.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut cache = Cache::default();
    let mut hisense_devices: BTreeMap<IpAddr, (Candidate, u64)> = BTreeMap::new();
    let mut buffer = [0; MAX_PACKET + 1];
    let mut buffer6 = [0; MAX_PACKET + 1];
    let mut hisense_buf = [0; 512];
    let mut packets = 0_u32;
    loop {
        tokio::select! {
            _ = refresh.tick() => {
                packets = 0;
                let now_ms = started.elapsed().as_millis() as u64;
                cache.expire(now_ms);
                hisense_devices.retain(|_, (_, expires_at)| *expires_at > now_ms);
                let query = query_with_resolution(&cache.resolution_queries())?;
                socket.send_to(&query, DESTINATION).await?;
                if let Some(socket6) = &socket6 { let _ = socket6.send_to(&query, DESTINATION_V6).await; }
                if let Some(udp) = &hisense_udp {
                    let target = SocketAddrV4::new(Ipv4Addr::BROADCAST, HISENSE_UDP_PORT);
                    let _ = udp.send_to(b"FindECloudBox:", target).await;
                    let _ = udp.send_to(b"Hisense516Pro", target).await;
                }
            }
            _ = expiry.tick() => {
                let now_ms = started.elapsed().as_millis() as u64;
                hisense_devices.retain(|_, (_, expires_at)| *expires_at > now_ms);
                let mut devices = cache.candidates(now_ms);
                for (ip, (candidate, _)) in &hisense_devices {
                    let already_present = devices
                        .iter()
                        .any(|d| d.endpoints.iter().any(|ep| ep.ip() == *ip));
                    if !already_present && devices.len() < MAX_CANDIDATES {
                        devices.push(candidate.clone());
                    }
                }
                devices.sort_by(|a, b| {
                    a.display_name
                        .cmp(&b.display_name)
                        .then_with(|| a.key.cmp(&b.key))
                });
                updates.send_if_modified(|state| { if state.devices == devices { false } else { state.devices = devices; true } });
            }
            packet = socket.recv_from(&mut buffer) => {
                let (length, source) = packet?;
                packets = packets.saturating_add(1);
                if packets <= 100 && source.port() == 5353 {
                    let _ = cache.ingest(&buffer[..length], started.elapsed().as_millis() as u64);
                }
            }
            packet = async {
                match &socket6 {
                    Some(socket6) => socket6.recv_from(&mut buffer6).await,
                    None => std::future::pending().await,
                }
            } => {
                // The IPv6 socket is optional; its errors never end the scan.
                let Ok((length, source)) = packet else { continue };
                packets = packets.saturating_add(1);
                if packets <= 100 && source.port() == 5353 {
                    let _ = cache.ingest(&buffer6[..length], started.elapsed().as_millis() as u64);
                }
            }
            packet = async {
                match &hisense_udp {
                    Some(udp) => udp.recv_from(&mut hisense_buf).await,
                    None => std::future::pending().await,
                }
            } => {
                let Ok((length, source)) = packet else { continue };
                packets = packets.saturating_add(1);
                if packets <= 100 && hisense_devices.len() < MAX_CANDIDATES
                    && let Some(candidate) = parse_hisense_broadcast(&hisense_buf[..length], source)
                {
                    let expires = (started.elapsed().as_millis() as u64).saturating_add(HISENSE_TTL_MS);
                    hisense_devices.insert(source.ip(), (candidate, expires));
                }
            }
        }
    }
}
