//! DNS-SD codec/cache plus a default-interface browser (IPv4, and IPv6 when
//! available) and an IPv4 responder.
//! DNS data is a candidate hint, never an authenticated peer identity.

use anyhow::{Context, Result, bail};
use hickory_proto::{
    op::{Message, MessageType, OpCode, Query},
    rr::{
        Name, RData, Record, RecordType,
        rdata::{A, PTR, SRV, TXT},
    },
};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6},
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    sync::watch,
    time::{Instant, MissedTickBehavior},
};

mod browser;
mod cache;
mod model;
mod protocols;
pub use browser::{BrowseFailure, BrowsePhase, Browser, DiscoverySnapshot, browse};
pub use cache::Cache;
#[doc(hidden)]
pub use cache::{MAX_PACKET, packet};
pub use model::{AdvertisedOutputs, Availability, Candidate, OutputKind, ReceiverProtocol};
pub use protocols::parse_hisense_broadcast;

/// Janus-native receiving is optional; Google Cast devices need no Janus install.
pub const SERVICE: &str = "_janus-cast._udp.local.";
pub const GOOGLE_CAST_SERVICE: &str = "_googlecast._tcp.local.";
/// AirPlay receivers (Apple TV, Macs, and third-party TV/board receivers).
pub const AIRPLAY_SERVICE: &str = "_airplay._tcp.local.";
pub const MAX_CANDIDATES: usize = 128;
const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const DESTINATION: SocketAddrV4 = SocketAddrV4::new(GROUP, 5353);
const GROUP_V6: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0xfb);
const DESTINATION_V6: SocketAddrV6 = SocketAddrV6::new(GROUP_V6, 5353, 0, 0);

pub fn query() -> Result<Vec<u8>> {
    query_with_resolution(&[])
}

fn query_with_resolution(queries: &[Query]) -> Result<Vec<u8>> {
    let mut message = Message::new(0, MessageType::Query, OpCode::Query);
    for service in [GOOGLE_CAST_SERVICE, AIRPLAY_SERVICE, SERVICE] {
        message.add_query(Query::query(Name::from_ascii(service)?, RecordType::PTR));
    }
    message.add_queries(queries.iter().cloned());
    Ok(message.to_vec()?)
}

pub fn advertisement(label: &str, address: SocketAddrV4, ready: bool, ttl: u32) -> Result<Vec<u8>> {
    if label.is_empty()
        || label.len() > 48
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || address.port() == 0
    {
        bail!("invalid advertisement label or port");
    }
    let service = Name::from_ascii(SERVICE)?;
    let instance = Name::from_ascii(format!("{label}.{SERVICE}"))?;
    let host = Name::from_ascii(format!("{label}.local."))?;
    let mut message = Message::response(0, OpCode::Query);
    message.metadata.authoritative = true;
    message.add_answer(Record::from_rdata(
        service,
        ttl,
        RData::PTR(PTR(instance.clone())),
    ));
    for mut record in [
        Record::from_rdata(
            instance.clone(),
            ttl,
            RData::SRV(SRV::new(0, 0, address.port(), host.clone())),
        ),
        Record::from_rdata(
            instance,
            ttl,
            RData::TXT(TXT::new(vec![
                "v=1".into(),
                format!("ready={}", u8::from(ready)),
            ])),
        ),
        Record::from_rdata(host, ttl, RData::A(A(*address.ip()))),
    ] {
        record.mdns_cache_flush = true;
        message.add_additional(record);
    }
    Ok(message.to_vec()?)
}

fn socket() -> Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 5353).into())?;
    socket.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED)?;
    socket.set_multicast_ttl_v4(255)?;
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into()).context("open mDNS socket")
}

/// Default-interface IPv6 mDNS. Some receivers announce a service over IPv6
/// only intermittently over IPv4 (observed: an AirPlay record), so browsing
/// listens on both; the caller treats failure here as IPv4-only operation.
fn socket_v6() -> Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_only_v6(true)?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.bind(&SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 5353, 0, 0).into())?;
    socket.join_multicast_v6(&GROUP_V6, 0)?;
    socket.set_multicast_hops_v6(255)?;
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into()).context("open IPv6 mDNS socket")
}

/// A watch update publishes availability changes. Dropping/sending true on stop
/// sends a best-effort goodbye. Advertise only a concretely bound LAN address.
pub async fn advertise(
    label: String,
    address: SocketAddrV4,
    mut ready: watch::Receiver<bool>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let socket = socket()?;
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let service = Name::from_ascii(SERVICE)?;
    let mut buffer = [0; MAX_PACKET + 1];
    let mut last_reply = Instant::now() - Duration::from_secs(1);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            update = ready.changed() => {
                if update.is_err() { break; }
                let bytes = advertisement(&label, address, *ready.borrow(), 120)?;
                socket.send_to(&bytes, DESTINATION).await?;
            }
            _ = interval.tick() => { let bytes = advertisement(&label, address, *ready.borrow(), 120)?;
                socket.send_to(&bytes, DESTINATION).await?; }
            result = socket.recv_from(&mut buffer) => {
                let (len, _) = result?;
                if last_reply.elapsed() < Duration::from_millis(100) { continue; }
                if let Ok(message) = packet(&buffer[..len])
                    && message.metadata.message_type == MessageType::Query
                    && message.queries.iter().any(|query| *query.name() == service) {
                        let bytes = advertisement(&label, address, *ready.borrow(), 120)?;
                socket.send_to(&bytes, DESTINATION).await?;
                        last_reply = Instant::now();
                }
            }
        }
    }
    let _ = socket
        .send_to(&advertisement(&label, address, false, 0)?, DESTINATION)
        .await;
    Ok(())
}
