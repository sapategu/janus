use crate::{Candidate, MAX_CANDIDATES};
use anyhow::{Result, bail};
use hickory_proto::{
    op::{Message, MessageType, Query},
    rr::{
        DNSClass, Name, RData, Record, RecordType,
        rdata::{A, PTR},
    },
};
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
};

const MAX_RECORDS: usize = 512;
pub const MAX_PACKET: usize = 9000;
const MAX_ENDPOINTS: usize = 8;

struct Cached {
    record: Record,
    expires: u64,
    received: u64,
}

#[derive(Default)]
pub struct Cache {
    records: Vec<Cached>,
}

pub fn packet(bytes: &[u8]) -> Result<Message> {
    if bytes.len() < 12 || bytes.len() > MAX_PACKET {
        bail!("invalid DNS packet length");
    }
    let count: usize = [4, 6, 8, 10]
        .into_iter()
        .map(|offset| u16::from_be_bytes([bytes[offset], bytes[offset + 1]]) as usize)
        .sum();
    if count > 128 {
        bail!("too many DNS records");
    }
    Ok(Message::from_vec(bytes)?)
}

impl Cache {
    pub fn ingest(&mut self, bytes: &[u8], now_ms: u64) -> Result<()> {
        let message = packet(bytes)?;
        if message.metadata.message_type != MessageType::Response || message.metadata.truncation {
            return Ok(());
        }
        self.expire(now_ms);
        let local = Name::from_ascii("local.").expect("constant domain");
        let records: Vec<_> = message
            .answers
            .into_iter()
            .chain(message.additionals)
            .filter(|record| record.dns_class == DNSClass::IN && local.zone_of(&record.name))
            .collect();
        let flushed: HashSet<_> = records
            .iter()
            .filter(|record| record.mdns_cache_flush && record.ttl > 0)
            .map(|record| (record.name.clone(), record.record_type()))
            .collect();
        // RFC 6762 cache-flush grace prevents one packet in a split RRset from
        // immediately deleting addresses carried by another recent packet.
        for cached in &mut self.records {
            if flushed.contains(&(cached.record.name.clone(), cached.record.record_type()))
                && now_ms.saturating_sub(cached.received) >= 1000
            {
                cached.expires = cached.expires.min(now_ms.saturating_add(1000));
            }
        }
        for record in records {
            self.records.retain(|cached| {
                !(cached.record.name == record.name && cached.record.data == record.data)
            });
            if record.ttl == 0 || self.records.len() >= MAX_RECORDS {
                continue;
            }
            let expires = now_ms.saturating_add(u64::from(record.ttl.min(120)) * 1000);
            self.records.push(Cached {
                record,
                expires,
                received: now_ms,
            });
        }
        Ok(())
    }

    pub fn expire(&mut self, now_ms: u64) {
        self.records.retain(|cached| cached.expires > now_ms);
    }
    pub fn record_count(&self) -> usize {
        self.records.len()
    }

    pub fn candidates(&mut self, now_ms: u64) -> Vec<Candidate> {
        self.expire(now_ms);
        let mut candidates = Vec::new();
        for ptr in &self.records {
            let Some(protocol) = crate::protocols::from_service(&ptr.record.name) else {
                continue;
            };
            let RData::PTR(PTR(instance)) = &ptr.record.data else {
                continue;
            };
            if !ptr.record.name.zone_of(instance) {
                continue;
            }
            let srv = self
                .records
                .iter()
                .rev()
                .find_map(|cached| match &cached.record.data {
                    RData::SRV(srv) if cached.record.name == *instance && srv.port > 0 => Some(srv),
                    _ => None,
                });
            let txt = self
                .records
                .iter()
                .rev()
                .find_map(|cached| match &cached.record.data {
                    RData::TXT(txt) if cached.record.name == *instance => Some(txt),
                    _ => None,
                });
            let (Some(srv), Some(txt)) = (srv, txt) else {
                continue;
            };
            let Some(metadata) = crate::protocols::metadata(protocol, txt, instance.iter().next())
            else {
                continue;
            };
            let mut endpoints: Vec<_> = self
                .records
                .iter()
                .filter_map(|cached| {
                    if cached.record.name != srv.target {
                        return None;
                    }
                    match cached.record.data {
                        RData::A(A(ip)) if ip.is_private() || ip.is_link_local() => {
                            Some(SocketAddr::new(IpAddr::V4(ip), srv.port))
                        }
                        // Link-local IPv6 must retain an interface index; implement
                        // the interface-aware browser before exposing those routes.
                        _ => None,
                    }
                })
                .collect();
            endpoints.sort();
            endpoints.dedup();
            endpoints.truncate(MAX_ENDPOINTS);
            if endpoints.is_empty() {
                continue;
            }
            let instance_text = instance.to_ascii();
            let candidate = Candidate {
                key: format!("{}:{}", protocol.key(), instance_text.to_ascii_lowercase()),
                protocol,
                instance: instance_text,
                advertised_id: metadata.advertised_id,
                display_name: metadata.name,
                model: metadata.model,
                endpoints,
                availability: metadata.availability,
                advertised_outputs: metadata.outputs,
            };
            if !candidates
                .iter()
                .any(|previous: &Candidate| previous.key == candidate.key)
            {
                candidates.push(candidate);
            }
            if candidates.len() == MAX_CANDIDATES {
                break;
            }
        }
        candidates.sort_by(|a, b| {
            a.display_name
                .cmp(&b.display_name)
                .then_with(|| a.key.cmp(&b.key))
        });
        candidates
    }

    /// Resolve split PTR/SRV/TXT/address responses without assuming that every
    /// responder puts a complete device description into one packet.
    #[doc(hidden)]
    pub fn resolution_queries(&self) -> Vec<Query> {
        let mut wanted = Vec::new();
        let mut seen = HashSet::new();
        let mut add = |name: &Name, kind| {
            if wanted.len() < 64 && seen.insert((name.clone(), kind)) {
                wanted.push(Query::query(name.clone(), kind));
            }
        };
        for cached in &self.records {
            if crate::protocols::from_service(&cached.record.name).is_none() {
                continue;
            }
            let RData::PTR(PTR(instance)) = &cached.record.data else {
                continue;
            };
            if !cached.record.name.zone_of(instance) {
                continue;
            }
            for kind in [RecordType::SRV, RecordType::TXT] {
                if !self.records.iter().any(|cached| {
                    cached.record.name == *instance && cached.record.record_type() == kind
                }) {
                    add(instance, kind);
                }
            }
            for cached in &self.records {
                if cached.record.name != *instance {
                    continue;
                }
                let RData::SRV(srv) = &cached.record.data else {
                    continue;
                };
                if !Name::from_ascii("local.")
                    .expect("constant domain")
                    .zone_of(&srv.target)
                {
                    continue;
                }
                if !self.records.iter().any(|cached| {
                    cached.record.name == srv.target && cached.record.record_type() == RecordType::A
                }) {
                    add(&srv.target, RecordType::A);
                }
            }
        }
        wanted
    }
}
