//! Cast RTP packetization and AES-128-CTR frame encryption. Wire layout and
//! nonce rules follow Open Screen `rtp_defines.h`, `rtp_packetizer.cc` and
//! `frame_crypto.cc` (commit 308a18e24e717ce27231c0aaa713364a41416e54).
use aes::Aes128;
use ctr::cipher::{KeyIvInit, StreamCipher};

/// Ethernet MTU minus IPv4 and UDP headers.
pub const MAX_PACKET_BYTES: usize = 1500 - 20 - 8;
/// 12-byte RTP header + 7-byte Cast header (always with reference frame ID).
pub const BASE_HEADER_BYTES: usize = 19;
const ADAPTIVE_LATENCY_BYTES: usize = 4;
/// Room for the optional latency extension so every packet fits.
pub const MAX_PAYLOAD_BYTES: usize = MAX_PACKET_BYTES - BASE_HEADER_BYTES - ADAPTIVE_LATENCY_BYTES;
/// FramePacketId 0xffff means "all packets lost" in feedback.
pub const ALL_PACKETS_LOST: u16 = 0xffff;
const MAX_PACKETS_PER_FRAME: usize = ALL_PACKETS_LOST as usize - 1;

const REQUIRED_FIRST_BYTE: u8 = 0b1000_0000;
const MARKER_BIT: u8 = 0b1000_0000;
const KEY_FRAME_BIT: u8 = 0b1000_0000;
const HAS_REFERENCE_FRAME_BIT: u8 = 0b0100_0000;
const ADAPTIVE_LATENCY_EXTENSION: u16 = 1;

/// A frame after encryption; `frame_id` is the full 64-bit value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncryptedFrame {
    pub frame_id: i64,
    pub referenced_frame_id: i64,
    pub key_frame: bool,
    pub rtp_timestamp: u32,
    /// Only set on the first frame or when the playout delay changes.
    pub new_playout_delay_ms: Option<u16>,
    pub data: Vec<u8>,
}

pub struct FrameCrypto {
    key: [u8; 16],
    iv_mask: [u8; 16],
}

impl FrameCrypto {
    pub fn new(key: [u8; 16], iv_mask: [u8; 16]) -> Self {
        Self { key, iv_mask }
    }

    /// Nonce = IV mask XOR (frame_id low 32 bits, big-endian, at bytes 8..12);
    /// full 128-bit big-endian counter (OpenSSL AES_ctr128_encrypt).
    pub fn crypt(&self, frame_id: i64, data: &mut [u8]) {
        let mut nonce = [0_u8; 16];
        nonce[8..12].copy_from_slice(&(frame_id as u32).to_be_bytes());
        for (byte, mask) in nonce.iter_mut().zip(self.iv_mask) {
            *byte ^= mask;
        }
        let mut cipher = ctr::Ctr128BE::<Aes128>::new(&self.key.into(), &nonce.into());
        cipher.apply_keystream(data);
    }
}

/// Number of packets for `payload_bytes`, at least one, or `None` if the frame
/// cannot be represented by 16-bit packet IDs.
pub fn packet_count(payload_bytes: usize) -> Option<usize> {
    let count = payload_bytes.div_ceil(MAX_PAYLOAD_BYTES).max(1);
    (count <= MAX_PACKETS_PER_FRAME).then_some(count)
}

pub struct Packetizer {
    payload_type: u8,
    ssrc: u32,
    sequence: u16,
}

impl Packetizer {
    pub fn new(payload_type: u8, ssrc: u32, first_sequence: u16) -> Self {
        Self {
            payload_type: payload_type & 0x7f,
            ssrc,
            sequence: first_sequence,
        }
    }

    /// Builds packet `packet_id` of `frame` into `out` (cleared first).
    pub fn packet(&mut self, frame: &EncryptedFrame, packet_id: u16, out: &mut Vec<u8>) {
        let count = packet_count(frame.data.len()).expect("frame size checked when enqueued");
        assert!(usize::from(packet_id) < count);
        let last = usize::from(packet_id) == count - 1;
        let latency = frame.new_playout_delay_ms.filter(|_| packet_id == 0);
        let start = usize::from(packet_id) * MAX_PAYLOAD_BYTES;
        let end = if last {
            frame.data.len()
        } else {
            start + MAX_PAYLOAD_BYTES
        };

        out.clear();
        out.push(REQUIRED_FIRST_BYTE);
        out.push(if last { MARKER_BIT } else { 0 } | self.payload_type);
        out.extend_from_slice(&self.sequence.to_be_bytes());
        self.sequence = self.sequence.wrapping_add(1);
        out.extend_from_slice(&frame.rtp_timestamp.to_be_bytes());
        out.extend_from_slice(&self.ssrc.to_be_bytes());
        out.push(
            if frame.key_frame { KEY_FRAME_BIT } else { 0 }
                | HAS_REFERENCE_FRAME_BIT
                | u8::from(latency.is_some()),
        );
        out.push(frame.frame_id as u8);
        out.extend_from_slice(&packet_id.to_be_bytes());
        out.extend_from_slice(&((count - 1) as u16).to_be_bytes());
        out.push(frame.referenced_frame_id as u8);
        if let Some(delay) = latency {
            // TYPE (6 bits) | SIZE (10 bits) = 2 bytes, then the delay.
            out.extend_from_slice(&((ADAPTIVE_LATENCY_EXTENSION << 10) | 2).to_be_bytes());
            out.extend_from_slice(&delay.to_be_bytes());
        }
        out.extend_from_slice(&frame.data[start..end]);
    }
}
