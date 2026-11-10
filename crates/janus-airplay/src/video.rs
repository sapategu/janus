//! AirPlay H.264 framing, ported from Doubletake 5e2f1cb/mirror.go.
//! Codec configuration is clear; VCL access units share one AES-CTR stream,
//! or (paired profile) each is sealed with ChaCha20-Poly1305 using the
//! 128-byte header as AAD and a per-frame counter nonce.
use crate::{
    Error, Profile,
    hap::{Aead, counter},
    session::derive,
};
use aes::{
    Aes128,
    cipher::{KeyIvInit, StreamCipher},
};
use ctr::Ctr128BE;
use std::time::Duration;
pub(crate) const MAX_FRAME: usize = 8 * 1024 * 1024;
enum Cipher {
    Ctr(Box<Ctr128BE<Aes128>>),
    ChaCha { key: Box<Aead>, nonce: u64 },
    Plain,
}
pub(crate) struct Video {
    cipher: Cipher,
    sps: Vec<u8>,
    pps: Vec<u8>,
    width: u32,
    height: u32,
    profile: Profile,
}
impl Video {
    pub(crate) fn new(key: [u8; 16], id: u64, width: u32, height: u32) -> Self {
        let k = derive(&format!("AirPlayStreamKey{id}"), &key);
        let iv = derive(&format!("AirPlayStreamIV{id}"), &key);
        Self::with_cipher(k, iv, width, height, Profile::Rtsp)
    }
    pub(crate) fn http(key: [u8; 16], iv: [u8; 16], width: u32, height: u32) -> Self {
        Self::with_cipher(key, iv, width, height, Profile::LegacyHttp)
    }
    pub(crate) fn hisense(width: u32, height: u32) -> Self {
        Self {
            cipher: Cipher::Plain,
            sps: Vec::new(),
            pps: Vec::new(),
            width,
            height,
            profile: Profile::Hisense,
        }
    }
    pub(crate) fn profile(&self) -> Profile {
        self.profile
    }
    /// Paired profile: `key` is HKDF(pair-verify secret, streamConnectionID).
    pub(crate) fn paired(key: [u8; 32], width: u32, height: u32) -> Self {
        Self {
            cipher: Cipher::ChaCha {
                key: Box::new(Aead::new(&key)),
                nonce: 0,
            },
            sps: Vec::new(),
            pps: Vec::new(),
            width,
            height,
            profile: Profile::Paired,
        }
    }
    fn with_cipher(key: [u8; 16], iv: [u8; 16], width: u32, height: u32, profile: Profile) -> Self {
        Self {
            cipher: Cipher::Ctr(Box::new(Ctr128BE::<Aes128>::new(&key.into(), &iv.into()))),
            sps: Vec::new(),
            pps: Vec::new(),
            width,
            height,
            profile,
        }
    }
    pub(crate) fn packets(
        &mut self,
        data: &[u8],
        key: bool,
        time: Duration,
    ) -> Result<Vec<Vec<u8>>, Error> {
        let nals = nals(data)?;
        let mut sps = None;
        let mut pps = None;
        let mut vcl = Vec::new();
        let mut annexb_vcl = Vec::new();
        for nal in nals {
            match nal[0] & 31 {
                7 => sps = Some(nal),
                8 => pps = Some(nal),
                1..=5 => {
                    if self.profile == Profile::Hisense {
                        annexb_vcl.extend_from_slice(&[0, 0, 0, 1]);
                        annexb_vcl.extend_from_slice(nal);
                    } else {
                        vcl.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                        vcl.extend_from_slice(nal);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        let changed = sps.is_some_and(|s| s != self.sps) || pps.is_some_and(|p| p != self.pps);
        if let Some(s) = sps {
            self.sps = s.to_vec();
        }
        if let Some(p) = pps {
            self.pps = p.to_vec();
        }
        if self.sps.len() < 4 || self.pps.is_empty() {
            return Err(Error::Protocol);
        }
        if self.profile == Profile::Hisense {
            let pts_us = u64::try_from(time.as_micros()).unwrap_or(u64::MAX);
            if changed || key {
                let mut config = Vec::with_capacity(8 + self.sps.len() + self.pps.len());
                config.extend_from_slice(&[0, 0, 0, 1]);
                config.extend_from_slice(&self.sps);
                config.extend_from_slice(&[0, 0, 0, 1]);
                config.extend_from_slice(&self.pps);
                let mut header = header(config.len(), pts_us);
                header[4..6].copy_from_slice(&0x0100u16.to_le_bytes());
                let mut packet = header.to_vec();
                packet.extend_from_slice(&config);
                out.push(packet);
            }
            if !annexb_vcl.is_empty() {
                let mut header = header(annexb_vcl.len(), pts_us);
                header[4..6].copy_from_slice(&0x0101u16.to_le_bytes());
                let mut packet = header.to_vec();
                packet.extend(annexb_vcl);
                out.push(packet);
            }
            return Ok(out);
        }
        let timestamp = timestamp(time);
        if changed {
            let mut config = vec![1, self.sps[1], self.sps[2], self.sps[3], 0xff, 0xe1];
            config.extend_from_slice(&(self.sps.len() as u16).to_be_bytes());
            config.extend_from_slice(&self.sps);
            config.push(1);
            config.extend_from_slice(&(self.pps.len() as u16).to_be_bytes());
            config.extend_from_slice(&self.pps);
            if self.profile != Profile::LegacyHttp {
                config.extend_from_slice(&[2, 0, 0, 0]);
            }
            let mut header = header(config.len(), timestamp);
            header[4] = 1;
            if self.profile != Profile::LegacyHttp {
                header[6] = 0x16;
                header[7] = 1;
            } else {
                header[6] = 6;
            }
            for (offset, value) in [
                (16, self.width),
                (20, self.height),
                (40, self.width),
                (44, self.height),
                (56, self.width),
                (60, self.height),
            ] {
                header[offset..offset + 4].copy_from_slice(&(value as f32).to_le_bytes());
            }
            let mut packet = header.to_vec();
            packet.extend_from_slice(&config);
            out.push(packet);
        }
        if !vcl.is_empty() {
            let tag = match self.cipher {
                Cipher::Ctr(_) | Cipher::Plain => 0,
                Cipher::ChaCha { .. } => 16,
            };
            let mut header = header(vcl.len() + tag, timestamp);
            if self.profile == Profile::LegacyHttp {
                header[6] = 6;
            } else if key {
                header[5] = 0x10;
            }
            match &mut self.cipher {
                Cipher::Ctr(cipher) => cipher.apply_keystream(&mut vcl),
                Cipher::ChaCha { key, nonce } => {
                    key.seal(counter(*nonce), &header, &mut vcl);
                    *nonce += 1;
                }
                Cipher::Plain => {}
            }
            let mut packet = header.to_vec();
            packet.extend(vcl);
            out.push(packet);
        }
        Ok(out)
    }
}
fn header(size: usize, time: u64) -> [u8; 128] {
    let mut h = [0; 128];
    h[..4].copy_from_slice(&(size as u32).to_le_bytes());
    h[8..16].copy_from_slice(&time.to_le_bytes());
    h
}
pub(crate) fn timestamp(d: Duration) -> u64 {
    (d.as_secs() << 32) | ((u64::from(d.subsec_nanos()) << 32) / 1_000_000_000)
}
fn nals(data: &[u8]) -> Result<Vec<&[u8]>, Error> {
    if data.len() > MAX_FRAME {
        return Err(Error::Protocol);
    }
    let mut starts = Vec::new();
    for i in 0..data.len().saturating_sub(2) {
        if data[i..i + 3] == [0, 0, 1] {
            let boundary = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            starts.push((boundary, i + 3));
            if starts.len() > 256 {
                return Err(Error::Protocol);
            }
        }
    }
    if starts.first().map(|s| s.0) != Some(0) {
        return Err(Error::Protocol);
    }
    let mut out = Vec::new();
    for (i, (_, start)) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(data.len(), |s| s.0);
        let nal = &data[*start..end];
        let last = nal.iter().rposition(|b| *b != 0).ok_or(Error::Protocol)?;
        let nal = &nal[..=last];
        if nal[0] & 0x80 != 0 || nal.len() > u16::MAX as usize && matches!(nal[0] & 31, 7 | 8) {
            return Err(Error::Protocol);
        }
        out.push(nal);
    }
    Ok(out)
}
#[doc(hidden)]
pub fn paired_vector(key: [u8; 32], data: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    Video::paired(key, 1280, 720).packets(data, true, Duration::from_millis(500))
}
#[doc(hidden)]
pub fn framed_vector(key: [u8; 16], id: u64, data: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    Video::new(key, id, 1280, 720).packets(data, true, Duration::from_millis(500))
}
#[doc(hidden)]
pub fn hisense_vector(data: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    Video::hisense(1920, 1080).packets(data, true, Duration::from_millis(500))
}
