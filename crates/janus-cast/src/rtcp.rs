//! Cast RTCP: the sender report Janus emits, and the receiver's compound
//! RTCP it parses (receiver report, Cast ACK/NACK feedback, picture loss).
//! Parsing ports Open Screen `cast/streaming/impl/rtp_wire.rs` and the
//! report/NTP rules of `sender_report_builder.cc` / `ntp_time.cc`
//! (BSD-style license, The Chromium Authors; commit 308a18e2).

const VERSION_BITS: u8 = 0b10;
const SENDER_REPORT: u8 = 200;
const RECEIVER_REPORT: u8 = 201;
const PAYLOAD_SPECIFIC: u8 = 206;
const PICTURE_LOSS_SUBTYPE: u8 = 1;
const FEEDBACK_SUBTYPE: u8 = 15;
const CAST: u32 = u32::from_be_bytes(*b"CAST");
const CST2: u32 = u32::from_be_bytes(*b"CST2");
const MAX_NACKS: usize = 4096;
const MAX_ACKS: usize = 2048;
/// Seconds between the NTP (1900) and Unix (1970) epochs.
pub const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

/// 64-bit NTP timestamp for `unix` (seconds, nanoseconds).
pub fn ntp_timestamp(unix_seconds: u64, nanos: u32) -> u64 {
    let seconds = unix_seconds + NTP_UNIX_OFFSET;
    let fraction = (u64::from(nanos) << 32) / 1_000_000_000;
    (seconds << 32) | fraction
}

/// The "status report id" a receiver echoes: middle 32 bits of the NTP time.
pub fn report_id(ntp: u64) -> u32 {
    (ntp >> 16) as u32
}

/// RTCP Sender Report without report blocks (28 bytes).
pub fn sender_report(
    ssrc: u32,
    ntp: u64,
    rtp_timestamp: u32,
    packets: u32,
    octets: u32,
) -> [u8; 28] {
    let mut out = [0_u8; 28];
    out[0] = VERSION_BITS << 6;
    out[1] = SENDER_REPORT;
    out[2..4].copy_from_slice(&6_u16.to_be_bytes()); // 24-byte payload / 4
    out[4..8].copy_from_slice(&ssrc.to_be_bytes());
    out[8..16].copy_from_slice(&ntp.to_be_bytes());
    out[16..20].copy_from_slice(&rtp_timestamp.to_be_bytes());
    out[20..24].copy_from_slice(&packets.to_be_bytes());
    out[24..28].copy_from_slice(&octets.to_be_bytes());
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiverReport {
    pub last_report_id: u32,
    /// 1/65536-second units.
    pub delay_since_last_report: u32,
    pub cumulative_lost: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PacketNack {
    pub frame_id: i64,
    /// [`crate::rtp::ALL_PACKETS_LOST`] means every packet of the frame.
    pub packet_id: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Feedback {
    pub report: Option<ReceiverReport>,
    /// All frames up to and including this one were received.
    pub checkpoint: Option<i64>,
    pub playout_delay_ms: u16,
    pub nacks: Vec<PacketNack>,
    /// Individually received frames after the checkpoint.
    pub acks: Vec<i64>,
    pub picture_loss: bool,
}

struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.data.len() < n {
            return None;
        }
        let (head, tail) = self.data.split_at(n);
        self.data = tail;
        Some(head)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

fn expand_at_most(reference: i64, low: u8) -> i64 {
    let mut value = (reference & !0xff) | i64::from(low);
    if value > reference {
        value -= 0x100;
    }
    value
}

fn expand_above(reference: i64, low: u8) -> i64 {
    expand_at_most(reference + 0x100, low)
}

/// Parses one compound RTCP datagram from the receiver for this stream.
/// `latest_frame_id` is the newest frame sent (bounds 8-bit expansion).
/// Returns `None` for malformed input.
pub fn parse(
    packet: &[u8],
    receiver_ssrc: u32,
    sender_ssrc: u32,
    latest_frame_id: i64,
) -> Option<Feedback> {
    let mut out = Feedback::default();
    let mut reader = Reader { data: packet };
    while !reader.data.is_empty() {
        let byte0 = reader.u8()?;
        if byte0 >> 5 != 0b100 {
            return None;
        }
        let count_or_subtype = byte0 & 0x1f;
        let packet_type = reader.u8()?;
        let words = usize::from(reader.u16()?);
        let mut body = Reader {
            data: reader.take(words * 4)?,
        };
        match (packet_type, count_or_subtype) {
            (RECEIVER_REPORT, count) => {
                if body.u32()? != receiver_ssrc {
                    continue;
                }
                for _ in 0..count {
                    let to = body.u32()?;
                    let lost = body.u32()? & 0x00ff_ffff;
                    body.take(8)?; // highest sequence, jitter
                    let last_report_id = body.u32()?;
                    let delay = body.u32()?;
                    if to == sender_ssrc {
                        out.report = Some(ReceiverReport {
                            last_report_id,
                            delay_since_last_report: delay,
                            cumulative_lost: lost,
                        });
                    }
                }
            }
            (PAYLOAD_SPECIFIC, PICTURE_LOSS_SUBTYPE) => {
                if body.u32()? == receiver_ssrc && body.u32()? == sender_ssrc {
                    out.picture_loss = true;
                }
            }
            (PAYLOAD_SPECIFIC, FEEDBACK_SUBTYPE) => {
                if body.u32()? != receiver_ssrc || body.u32()? != sender_ssrc {
                    continue;
                }
                if body.u32()? != CAST {
                    return None;
                }
                let checkpoint = expand_at_most(latest_frame_id, body.u8()?);
                let losses = usize::from(body.u8()?);
                let delay = body.u16()?;
                if out.checkpoint.is_some_and(|previous| previous > checkpoint) {
                    continue;
                }
                out.checkpoint = Some(checkpoint);
                out.playout_delay_ms = delay;
                out.nacks.clear();
                out.acks.clear();
                for _ in 0..losses {
                    let frame_id = expand_above(checkpoint, body.u8()?);
                    let mut packet_id = body.u16()?;
                    let mut bits = body.u8()?;
                    out.nacks.push(PacketNack {
                        frame_id,
                        packet_id,
                    });
                    if packet_id != crate::rtp::ALL_PACKETS_LOST {
                        while bits != 0 {
                            packet_id = packet_id.wrapping_add(1);
                            if bits & 1 != 0 {
                                out.nacks.push(PacketNack {
                                    frame_id,
                                    packet_id,
                                });
                            }
                            bits >>= 1;
                        }
                    }
                    if out.nacks.len() > MAX_NACKS {
                        return None;
                    }
                }
                if body.data.len() >= 6 && body.u32()? == CST2 {
                    body.u8()?; // feedback count
                    let octets = usize::from(body.u8()?);
                    let bitmap = body.take(octets)?;
                    let mut first = checkpoint + 2;
                    for byte in bitmap {
                        for bit in 0..8 {
                            if byte & (1 << bit) != 0 {
                                out.acks.push(first + bit);
                            }
                        }
                        first += 8;
                    }
                    if out.acks.len() > MAX_ACKS {
                        return None;
                    }
                }
            }
            _ => {} // sender reports, app-defined logs, extended reports: ignored
        }
    }
    // Canonical order; an all-lost NACK subsumes individual ones.
    out.nacks
        .sort_by_key(|nack| (nack.frame_id, nack.packet_id.wrapping_add(1)));
    out.nacks.dedup_by(|later, earlier| {
        earlier.frame_id == later.frame_id
            && (earlier.packet_id == crate::rtp::ALL_PACKETS_LOST
                || earlier.packet_id == later.packet_id)
    });
    out.acks.sort_unstable();
    out.acks.dedup();
    Some(out)
}
