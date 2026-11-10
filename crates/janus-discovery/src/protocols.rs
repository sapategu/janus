//! Native protocol metadata decoders. No SDK, browser or cloud calls.
use crate::{
    AIRPLAY_SERVICE, AdvertisedOutputs, Availability, Candidate, GOOGLE_CAST_SERVICE,
    ReceiverProtocol, SERVICE,
};
use std::net::{IpAddr, SocketAddr};

/// AirPlay `features` bits (public receiver documentation, e.g. nto's
/// unofficial AirPlay specification): video playback, screen mirroring, audio.
const AIRPLAY_VIDEO: u64 = 1 << 0;
const AIRPLAY_SCREEN: u64 = 1 << 7;
const AIRPLAY_AUDIO: u64 = 1 << 9;

/// `features` is `0xLOW` or `0xLOW,0xHIGH` (64-bit split in two words).
fn airplay_features(value: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(value).ok()?;
    let mut words = text.split(',');
    let word = |part: &str| {
        u64::from_str_radix(
            part.trim()
                .trim_start_matches("0x")
                .trim_start_matches("0X"),
            16,
        )
        .ok()
    };
    let low = word(words.next()?)?;
    let high = match words.next() {
        Some(part) => word(part)?,
        None => 0,
    };
    (low <= u64::from(u32::MAX) && high <= u64::from(u32::MAX) && words.next().is_none())
        .then_some(low | (high << 32))
}
use anyhow::{Result, bail};
use hickory_proto::rr::{Name, rdata::TXT};
use std::collections::HashMap;

pub(crate) struct Metadata {
    pub name: String,
    pub model: Option<String>,
    pub advertised_id: Option<String>,
    pub availability: Availability,
    pub outputs: AdvertisedOutputs,
}

pub(crate) fn from_service(name: &Name) -> Option<ReceiverProtocol> {
    if *name == Name::from_ascii(GOOGLE_CAST_SERVICE).expect("constant service") {
        Some(ReceiverProtocol::GoogleCast)
    } else if *name == Name::from_ascii(AIRPLAY_SERVICE).expect("constant service") {
        Some(ReceiverProtocol::AirPlay)
    } else if *name == Name::from_ascii(SERVICE).expect("constant service") {
        Some(ReceiverProtocol::JanusMoq)
    } else {
        None
    }
}

fn clean_label(value: &[u8]) -> Option<String> {
    let value = std::str::from_utf8(value).ok()?;
    let text: String = value.chars().filter(|c| !c.is_control() && !matches!(*c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')).take(64).collect();
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

fn text_fields(txt: &TXT) -> Result<HashMap<&[u8], &[u8]>> {
    if txt.txt_data.len() > 32 || txt.txt_data.iter().map(|data| data.len()).sum::<usize>() > 2048 {
        bail!("TXT record too large");
    }
    let mut result = HashMap::new();
    for data in &txt.txt_data {
        let Some(equal) = data.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let (key, rest) = data.split_at(equal);
        if key.is_empty() || key.len() > 16 || !key.iter().all(u8::is_ascii_lowercase) {
            continue;
        }
        let value = &rest[1..];
        if result
            .insert(key, value)
            .is_some_and(|previous| previous != value)
        {
            bail!("conflicting TXT keys");
        }
    }
    Ok(result)
}

pub(crate) fn metadata(
    protocol: ReceiverProtocol,
    txt: &TXT,
    fallback: Option<&[u8]>,
) -> Option<Metadata> {
    let fields = text_fields(txt).ok()?;
    let name = fields
        .get(b"fn".as_slice())
        .and_then(|value| clean_label(value));
    let model = fields
        .get(b"md".as_slice())
        .or_else(|| {
            (protocol == ReceiverProtocol::AirPlay)
                .then(|| fields.get(b"model".as_slice()))
                .flatten()
        })
        .and_then(|value| clean_label(value));
    let id_key: &[u8] = if protocol == ReceiverProtocol::AirPlay {
        b"deviceid"
    } else {
        b"id"
    };
    let advertised_id = fields.get(id_key).and_then(|value| {
        if value.is_empty()
            || value.len() > 64
            || !value
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
        {
            return None;
        }
        std::str::from_utf8(value).ok().map(str::to_owned)
    });
    let (name, availability, outputs) = match protocol {
        ReceiverProtocol::GoogleCast => {
            // Presence is not authentication; connection-time validation remains mandatory.
            advertised_id.as_ref()?;
            let name = name?;
            let bits = fields.get(b"ca".as_slice()).and_then(|value| {
                if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
                    return None;
                }
                std::str::from_utf8(value).ok()?.parse::<u64>().ok()
            });
            (
                name,
                Availability::Unknown,
                AdvertisedOutputs {
                    video: bits.map(|bits| bits & 1 != 0),
                    audio: bits.map(|bits| bits & 4 != 0),
                },
            )
        }
        ReceiverProtocol::AirPlay => {
            // The instance label is the friendly name; `features` must parse.
            let bits = fields
                .get(b"features".as_slice())
                .and_then(|value| airplay_features(value))?;
            (
                fallback.and_then(clean_label)?,
                Availability::Unknown,
                AdvertisedOutputs {
                    video: Some(bits & (AIRPLAY_SCREEN | AIRPLAY_VIDEO) != 0),
                    audio: Some(bits & AIRPLAY_AUDIO != 0),
                },
            )
        }
        ReceiverProtocol::JanusMoq => {
            if fields.get(b"v".as_slice()) != Some(&b"1".as_slice()) {
                return None;
            }
            let availability = match fields.get(b"ready".as_slice()).copied() {
                Some(b"1") => Availability::AdvertisedReady,
                Some(b"0") => Availability::AdvertisedBusy,
                _ => Availability::Unknown,
            };
            (
                name.or_else(|| fallback.and_then(clean_label))
                    .unwrap_or_else(|| "Janus receiver".into()),
                availability,
                AdvertisedOutputs::default(),
            )
        }
    };
    Some(Metadata {
        name,
        model,
        advertised_id,
        availability,
        outputs,
    })
}

fn valid_lan_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

/// Decodes a Hisense / EShare UDP 48689 broadcast announcement into a discovery
/// candidate. Never trusts public IP addresses or control characters.
pub fn parse_hisense_broadcast(packet: &[u8], source: SocketAddr) -> Option<Candidate> {
    if packet.is_empty() || packet.len() > 512 || !valid_lan_ip(source.ip()) {
        return None;
    }
    if packet.len() >= 50 && &packet[..9] == b"ECloudBox" && packet[9] != b':' {
        let end = packet[12..44]
            .iter()
            .position(|b| *b == 0)
            .map_or(44, |p| 12 + p);
        let name = clean_label(&packet[12..end])?;
        let id_slug: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        return Some(Candidate {
            key: format!("airplay:hisense-{id_slug}-{}", source.ip()),
            protocol: ReceiverProtocol::AirPlay,
            instance: format!("{name}._airplay._tcp.local."),
            advertised_id: None,
            display_name: name,
            model: Some("Hisense Interactive Display".into()),
            endpoints: vec![SocketAddr::new(source.ip(), 51010)],
            availability: Availability::AdvertisedReady,
            advertised_outputs: AdvertisedOutputs {
                video: Some(true),
                audio: Some(true),
            },
        });
    }
    let text = std::str::from_utf8(packet).ok()?.trim();
    let rest = text
        .strip_prefix("Hisense516Pro:")
        .or_else(|| text.strip_prefix("ECloudBox:"))?;
    let mut parts = rest.split(':');
    let _version = parts.next()?;
    let raw_name = parts.next()?;
    let name = clean_label(raw_name.as_bytes())?;
    let _web_port = parts.next();
    let airplay_port = parts
        .next()
        .and_then(|p| p.trim().parse::<u16>().ok())
        .filter(|p| *p > 0)
        .unwrap_or(51010);
    let serial = parts
        .next()
        .and_then(|s| s.strip_prefix("sn=").or(Some(s)))
        .and_then(|s| clean_label(s.as_bytes()))
        .filter(|s| {
            s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':'))
        });
    let id_slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    Some(Candidate {
        key: format!("airplay:hisense-{id_slug}-{}", source.ip()),
        protocol: ReceiverProtocol::AirPlay,
        instance: format!("{name}._airplay._tcp.local."),
        advertised_id: serial,
        display_name: name,
        model: Some("Hisense Interactive Display".into()),
        endpoints: vec![SocketAddr::new(source.ip(), airplay_port)],
        availability: Availability::AdvertisedReady,
        advertised_outputs: AdvertisedOutputs {
            video: Some(true),
            audio: Some(true),
        },
    })
}
