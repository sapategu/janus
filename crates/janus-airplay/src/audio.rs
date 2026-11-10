//! ALAC escape frames with legacy RTP/AES-CBC or, on the paired profile,
//! ChaCha20-Poly1305. ALAC syntax follows Apple's open-source ALACEncoder.cpp
//! EncodeStereoEscape (macosforge/alac). RTP, CBC, ChaCha and TimeAnnounce
//! follow Doubletake 5e2f1cb/audio.go: the paired cipher is the 64-bit-nonce
//! ChaCha20-Poly1305 of github.com/aead/chacha20poly1305 `NewCipher` (RFC 7539
//! MAC layout), which equals the IETF construction with nonce 0⁴ ‖ LE64(n).
//! AAD is the RTP timestamp and SSRC; the nonce trails the packet.
use crate::{
    Error,
    hap::{Aead, counter},
};
use aes::{
    Aes128,
    cipher::{BlockEncrypt, KeyInit},
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
pub const SAMPLES: usize = 352;
enum Cipher {
    Cbc { cipher: Box<Aes128>, iv: [u8; 16] },
    ChaCha { key: Box<Aead>, nonce: u64 },
}
pub(crate) struct Audio {
    cipher: Cipher,
    ssrc: u32,
    seq: u16,
}
impl Audio {
    pub(crate) fn new(key: [u8; 16], iv: [u8; 16], ssrc: u32, seq: u16) -> Self {
        Self {
            cipher: Cipher::Cbc {
                cipher: Box::new(Aes128::new(&key.into())),
                iv,
            },
            ssrc,
            seq,
        }
    }
    /// Paired profile: `key` is the random screen-audio key sent as `shk`.
    pub(crate) fn paired(key: [u8; 32], ssrc: u32, seq: u16) -> Self {
        Self {
            cipher: Cipher::ChaCha {
                key: Box::new(Aead::new(&key)),
                nonce: 0,
            },
            ssrc,
            seq,
        }
    }
    pub(crate) fn packet(&mut self, samples: &[i16], time: u32) -> Result<Vec<u8>, Error> {
        let payload = alac(samples)?;
        let mut packet = vec![0x80, 96];
        packet.extend_from_slice(&self.seq.to_be_bytes());
        packet.extend_from_slice(&time.to_be_bytes());
        packet.extend_from_slice(&self.ssrc.to_be_bytes());
        match &mut self.cipher {
            Cipher::Cbc { cipher, iv } => {
                packet.extend_from_slice(&payload);
                let mut chain = *iv;
                for bytes in packet[12..].chunks_exact_mut(16) {
                    let mut block = aes::cipher::Block::<Aes128>::default();
                    for i in 0..16 {
                        block[i] = bytes[i] ^ chain[i];
                    }
                    cipher.encrypt_block(&mut block);
                    bytes.copy_from_slice(&block);
                    chain.copy_from_slice(&block);
                }
            }
            Cipher::ChaCha { key, nonce } => {
                let mut sealed = payload;
                key.seal(counter(*nonce), &packet[4..12], &mut sealed);
                packet.extend_from_slice(&sealed);
                packet.extend_from_slice(&nonce.to_le_bytes());
                *nonce += 1;
            }
        }
        self.seq = self.seq.wrapping_add(1);
        Ok(packet)
    }
}
/// 44.1 kHz, stereo signed-16-bit ALAC escape frame. A fixed 352-sample frame
/// avoids a new codec dependency; bandwidth is ~1.42 Mb/s, not compressed ALAC.
pub fn alac(samples: &[i16]) -> Result<Vec<u8>, Error> {
    if samples.len() != SAMPLES * 2 {
        return Err(Error::Protocol);
    }
    let mut bytes = vec![0; (23 + samples.len() * 16 + 3).div_ceil(8)];
    let mut at = 0usize;
    let mut write = |value: u32, bits: usize| {
        for bit in (0..bits).rev() {
            if value & (1 << bit) != 0 {
                bytes[at / 8] |= 1 << (7 - at % 8);
            }
            at += 1;
        }
    };
    write(1, 3);
    write(0, 4);
    write(0, 12);
    write(1, 4);
    for sample in samples {
        write(u32::from(*sample as u16), 16);
    }
    write(7, 3);
    Ok(bytes)
}
pub(crate) fn sync(time: u32, ntp: u64, latency: u32, first: bool) -> [u8; 20] {
    let mut out = [0; 20];
    out[0] = if first { 0x90 } else { 0x80 };
    out[1] = 0xd4;
    out[2..4].copy_from_slice(&4u16.to_be_bytes());
    let play = time.wrapping_sub(latency);
    out[4..8].copy_from_slice(&play.to_be_bytes());
    out[8..16].copy_from_slice(&ntp.to_be_bytes());
    out[16..20].copy_from_slice(&play.to_be_bytes());
    out
}

/// Receiver-rule retransmission, bounded in both retained data and responses.
pub(crate) struct History {
    packets: VecDeque<Vec<u8>>,
    budget: usize,
    epoch: Instant,
}
impl History {
    pub(crate) fn new() -> Self {
        Self {
            packets: VecDeque::new(),
            budget: 128,
            epoch: Instant::now(),
        }
    }
    pub(crate) fn remember(&mut self, packet: Vec<u8>) -> Result<(), Error> {
        if !(12..=1472).contains(&packet.len()) {
            return Err(Error::Protocol);
        }
        self.packets.push_back(packet);
        if self.packets.len() > 256 {
            self.packets.pop_front();
        }
        Ok(())
    }
    pub(crate) fn responses(&mut self, request: &[u8]) -> Vec<Vec<u8>> {
        if request.len() != 8 || request[..2] != [0x80, 0xd5] {
            return Vec::new();
        }
        if self.epoch.elapsed() >= Duration::from_secs(1) {
            self.epoch = Instant::now();
            self.budget = 128;
        }
        let first = u16::from_be_bytes([request[4], request[5]]);
        let count = usize::from(u16::from_be_bytes([request[6], request[7]]))
            .min(32)
            .min(self.budget);
        let mut out = Vec::new();
        for offset in 0..count {
            let seq = first.wrapping_add(offset as u16);
            let mut response = vec![0x80, 0xd6, request[2], request[3]];
            self.budget -= 1;
            if let Some(packet) = self.packets.iter().find(|p| p[2..4] == seq.to_be_bytes()) {
                response.extend_from_slice(packet);
                out.push(response);
            } else {
                // An expired packet gets the protocol's short futile reply.
                response.extend_from_slice(&seq.to_be_bytes());
                response.extend_from_slice(&[0, 0]);
                out.push(response);
                break;
            }
        }
        out
    }
}
