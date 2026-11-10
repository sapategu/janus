// SPDX-License-Identifier: LGPL-3.0-only
// Native Rust port of Doubletake 5e2f1cb: fpsap.go, fairplay_crypto.go,
// fairplay_md5.go and fairplay_message.go. See ../PROVENANCE.md.
#![allow(clippy::needless_range_loop)]
use crate::{Error, sap::fairplay_sap_hash, tables::*};
use aes::{
    Aes128,
    cipher::{BlockEncrypt, KeyInit},
};
use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};

const INITIAL: [u8; 16] = [
    0xdc, 0xdc, 0xf3, 0xb9, 0x0b, 0x74, 0xdc, 0xfb, 0x86, 0x7f, 0xf7, 0x60, 0x16, 0x72, 0x90, 0x51,
];
const PREFIX: [u8; 17] = [
    0xa0, 0x44, 0x9c, 0x4d, 9, 0xe4, 0xbd, 0x7f, 0x6e, 0xc5, 0xd0, 0xcc, 0x35, 0x9d, 0xa7, 0x46,
    0x7a,
];
const SUFFIX: [u8; 16] = [
    0x97, 0xb5, 0x0f, 0x84, 0xe2, 0x15, 0x5a, 0x9c, 0x24, 0x99, 0x1c, 0xf4, 0x3a, 9, 0x63, 0x55,
];

pub(crate) fn random<const N: usize>() -> Result<[u8; N], Error> {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Error::Entropy)?;
    Ok(bytes)
}

pub(crate) struct FairPlay {
    local: [u8; 128],
    remote: [u8; 128],
    m3: Option<Vec<u8>>,
    confirmed: bool,
}
impl FairPlay {
    pub(crate) fn new() -> Result<Self, Error> {
        let mut local = random()?;
        local[0] = 0;
        local[1] = 1;
        Ok(Self {
            local,
            remote: [0; 128],
            m3: None,
            confirmed: false,
        })
    }
    pub(crate) fn message1() -> Vec<u8> {
        let mut r = record(1, 4);
        r[12..].copy_from_slice(&[2, 0, 3, 0xbb]);
        r
    }
    pub(crate) fn exchange(&mut self, m2: &[u8]) -> Result<Vec<u8>, Error> {
        if self.m3.is_some() {
            return Err(Error::Authentication);
        }
        validate(m2, 2, 130)?;
        if m2[12] != 2 || m2[13] > 3 {
            return Err(Error::Authentication);
        }
        let mode = m2[13] as usize;
        self.remote = message_crypt(
            mode,
            m2[14..].try_into().map_err(|_| Error::Authentication)?,
            false,
        );
        let mut m3 = record(3, 152);
        m3[12] = m2[13];
        m3[13..16].copy_from_slice(&[0x8f, 0x1a, 0x9c]);
        m3[16..144].copy_from_slice(&message_crypt(mode, &self.local, true));
        m3[144..].copy_from_slice(&exchange_seed(descriptor(&self.local, &self.remote)));
        self.m3 = Some(m3.clone());
        Ok(m3)
    }
    pub(crate) fn confirm(&mut self, m4: &[u8]) -> Result<(), Error> {
        validate(m4, 4, 20)?;
        let m3 = self.m3.as_ref().ok_or(Error::Authentication)?;
        if m4[12..] != m3[144..] {
            return Err(Error::Authentication);
        }
        self.confirmed = true;
        Ok(())
    }
    pub(crate) fn wrap(&self, key: [u8; 16]) -> Result<[u8; 72], Error> {
        self.wrap_mask(key, random()?)
    }
    fn wrap_mask(&self, key: [u8; 16], mask: [u8; 16]) -> Result<[u8; 72], Error> {
        if !self.confirmed {
            return Err(Error::Authentication);
        }
        let mut r = [0u8; 72];
        r[..16].copy_from_slice(b"FPLY\x01\x02\x01\0\0\0\0\x3c\0\0\0\0");
        r[16..32].copy_from_slice(&mask);
        r[32..36].copy_from_slice(&16u32.to_be_bytes());
        let wrapping = wrapping_key(&self.local, &self.remote);
        let mut block = aes::cipher::Block::<Aes128>::default();
        for i in 0..16 {
            block[i] = key[i] ^ mask[i];
        }
        Aes128::new(&wrapping.into()).encrypt_block(&mut block);
        r[56..].copy_from_slice(&block);
        let mac_key = hmac::Key::new(
            hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
            &descriptor(&self.local, &self.remote),
        );
        let mut mac = hmac::Context::with_key(&mac_key);
        mac.update(&r[..36]);
        mac.update(&key);
        r[36..56].copy_from_slice(mac.sign().as_ref());
        Ok(r)
    }
}
fn validate(r: &[u8], kind: u8, len: usize) -> Result<(), Error> {
    if r.len() != len + 12
        || &r[..4] != b"FPLY"
        || r[4..8] != [3, 1, kind, 0]
        || r[8..12] != (len as u32).to_be_bytes()
    {
        return Err(Error::Authentication);
    }
    Ok(())
}
fn record(kind: u8, len: usize) -> Vec<u8> {
    let mut r = vec![0; len + 12];
    r[..4].copy_from_slice(b"FPLY");
    r[4..8].copy_from_slice(&[3, 1, kind, 0]);
    r[8..12].copy_from_slice(&(len as u32).to_be_bytes());
    r
}

#[derive(Clone, Copy)]
enum Mutation {
    Swap,
    Cycle,
    Kdf,
}
fn compress(state: [u32; 4], block: &[u8], mutation: Mutation) -> [u32; 4] {
    let mut message = [0u32; 16];
    for i in 0..16 {
        message[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().expect("fixed block"));
    }
    let [mut a, mut b, mut c, mut d] = state;
    const SHIFTS: [[u32; 4]; 4] = [
        [7, 12, 17, 22],
        [5, 9, 14, 20],
        [4, 11, 16, 23],
        [6, 10, 15, 21],
    ];
    for round in 0..64 {
        let (f, word) = match round {
            0..16 => ((b & c) | (!b & d), round),
            16..32 => ((d & b) | (!d & c), (5 * round + 1) & 15),
            32..48 => (b ^ c ^ d, (3 * round + 5) & 15),
            _ => (c ^ (b | !d), (7 * round) & 15),
        };
        (a, b, c, d) = (
            d,
            b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(MD5_CONSTANT[round])
                    .wrapping_add(message[word])
                    .rotate_left(SHIFTS[round / 16][round % 4]),
            ),
            b,
            c,
        );
        if round == 31 {
            let indices = [
                a & 15,
                b & 15,
                c & 15,
                d & 15,
                (a >> 4) & 15,
                (b >> 4) & 15,
                (c >> 4) & 15,
                (d >> 4) & 15,
            ]
            .map(|i| i as usize);
            match mutation {
                Mutation::Swap => {
                    for (i, j) in indices.into_iter().enumerate() {
                        message.swap(i, j);
                    }
                }
                Mutation::Cycle => {
                    let first = message[indices[0]];
                    for i in 0..7 {
                        message[indices[i]] = message[indices[i + 1]];
                    }
                    message[indices[7]] = first;
                }
                Mutation::Kdf => {
                    message.swap((a & 15) as usize, (b & 15) as usize);
                    message.swap((c & 15) as usize, (d & 15) as usize);
                    for shift in [4, 8, 12] {
                        message.swap(((a >> shift) & 15) as usize, ((b >> shift) & 15) as usize);
                    }
                }
            }
        }
    }
    [
        state[0].wrapping_add(a),
        state[1].wrapping_add(b),
        state[2].wrapping_add(c),
        state[3].wrapping_add(d),
    ]
}
fn words_le(bytes: [u8; 16]) -> [u32; 4] {
    std::array::from_fn(|i| {
        u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().expect("fixed words"))
    })
}
fn words_be(words: [u32; 4]) -> [u8; 16] {
    let mut out = [0; 16];
    for (i, w) in words.into_iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
    }
    out
}
fn padded(prefix: &[u8], local: &[u8; 128], remote: &[u8; 128], suffix: &[u8]) -> [u8; 320] {
    let mut out = [0; 320];
    let mut n = 0;
    for data in [prefix, local.as_slice(), remote.as_slice(), suffix] {
        out[n..n + data.len()].copy_from_slice(data);
        n += data.len();
    }
    out[n] = 0x80;
    out[312..].copy_from_slice(&((n * 8) as u64).to_le_bytes());
    out
}
pub(crate) fn descriptor(local: &[u8; 128], remote: &[u8; 128]) -> [u8; 20] {
    let mut suffix = SUFFIX.to_vec();
    suffix.push(0x47);
    let bytes = padded(&PREFIX, local, remote, &suffix);
    let mut state = words_le(INITIAL);
    let mut first = [0; 4];
    for (i, block) in bytes.chunks_exact(64).enumerate() {
        let hash = words_le(fairplay_sap_hash(block.try_into().expect("fixed block")));
        for j in 0..4 {
            state[j] = state[j].wrapping_add(hash[j]);
        }
        state = compress(state, block, Mutation::Cycle);
        if i == 4 {
            first = state;
            state = compress(state, block, Mutation::Cycle);
        }
    }
    let mut out = [0; 20];
    out[..4].copy_from_slice(&first[0].to_be_bytes());
    out[4..].copy_from_slice(&words_be(state));
    out
}
fn wrapping_key(local: &[u8; 128], remote: &[u8; 128]) -> [u8; 16] {
    const P: [u8; 17] = [
        0xfa, 0x9c, 0xad, 0x4d, 0x4b, 0x68, 0x26, 0x8c, 0x7f, 0xf3, 0x88, 0x99, 0xde, 0x92, 0x2e,
        0x95, 0x1e,
    ];
    const S: [u8; 17] = [
        0xec, 0x4e, 0x27, 0x5e, 0xfd, 0xf2, 0xe8, 0x30, 0x97, 0xae, 0x70, 0xfb, 0xe0, 0, 0x3f,
        0x1c, 0x39,
    ];
    let bytes = padded(&P, local, remote, &S);
    let mut state = words_le(INITIAL);
    for block in bytes.chunks_exact(64) {
        let modified = compress(state, block, Mutation::Kdf);
        let hash = words_le(fairplay_sap_hash(block.try_into().expect("fixed block")));
        for i in 0..4 {
            state[i] = modified[i].wrapping_add(hash[i]);
        }
    }
    words_be(state)
}
fn digest32(left: [u8; 16], right: [u8; 16]) -> [u8; 16] {
    let mut block = [0; 64];
    block[..16].copy_from_slice(&left);
    block[16..32].copy_from_slice(&right);
    block[32] = 0x80;
    block[56..60].copy_from_slice(&0x100u32.to_le_bytes());
    words_be(compress(
        [0xb9f3dcdc, 0xfbdc740b, 0x60f77f86, 0x51907216],
        &block,
        Mutation::Swap,
    ))
}
fn substitute(lookup: [u8; 3], v: u8, mix: bool) -> u8 {
    let [table, input, output] = lookup;
    if mix {
        MIXES[table as usize][(v ^ input) as usize] ^ output
    } else {
        SUBSTITUTIONS[table as usize][(v ^ input) as usize] ^ output
    }
}
fn network(mut state: [u8; 16], masks: &[[u8; 16]; 9], second: bool) -> [u8; 16] {
    const FORWARD: [usize; 16] = [0, 5, 10, 15, 4, 9, 14, 3, 8, 13, 2, 7, 12, 1, 6, 11];
    const REVERSE: [usize; 16] = [0, 13, 10, 7, 4, 1, 14, 11, 8, 5, 2, 15, 12, 9, 6, 3];
    let (tables, map) = if second {
        (&SECOND, REVERSE)
    } else {
        (&FIRST, FORWARD)
    };
    for n in 0..9 {
        let bank = if second { 8 - n } else { n };
        let mut sub = [0; 16];
        for (output, input) in map.into_iter().enumerate() {
            sub[output] = substitute(
                tables[bank * 16 + if second { output } else { input }],
                state[input],
                false,
            );
            if second {
                sub[output] ^= masks[bank][output];
            }
        }
        for word in 0..4 {
            for output in 0..4 {
                state[word * 4 + output] = 0;
                for input in 0..4 {
                    state[word * 4 + output] ^= substitute(
                        tables[144 + input * 4 + output],
                        sub[word * 4 + input],
                        true,
                    );
                }
            }
        }
        if !second {
            for i in 0..16 {
                state[i] ^= masks[bank][i];
            }
        }
    }
    let mut out = [0; 16];
    for (output, input) in map.into_iter().enumerate() {
        out[output] = substitute(
            tables[160 + if second { output } else { input }],
            state[input],
            false,
        );
        if second {
            out[output] ^= OUTPUT_MASK[output];
        }
    }
    out
}
fn exchange_seed(seed: [u8; 20]) -> [u8; 20] {
    let mut masks = [[0; 16]; 9];
    for i in 0..9 {
        let mut block = [0; 64];
        block[..20].copy_from_slice(&seed);
        block[20] = i as u8;
        block[21..36].copy_from_slice(&[
            0x57, 0xd8, 0xee, 0xcb, 0xde, 0xfb, 0xcf, 0x59, 0x1c, 0x27, 0xa2, 0xcf, 0xbe, 0xb0,
            0x89,
        ]);
        block[36] = 0x80;
        block[56..60].copy_from_slice(&0x320u32.to_le_bytes());
        masks[i] = words_be(compress(
            [0x1d4a4587, 0x92f39fcc, 0x1d87d836, 0xcdc86697],
            &block,
            Mutation::Swap,
        ));
    }
    let fixed = [
        0xaf, 0xc2, 0x2b, 0xa0, 0x49, 0xef, 0xfc, 0xfb, 0xfe, 0x67, 0xac, 0x5e, 0xbe, 0xf6, 0xfb,
        0xcb,
    ];
    let first = network(
        std::array::from_fn(|i| fixed[i] ^ INPUT_MASK[i]),
        &masks,
        false,
    );
    let left = digest32(first, fixed);
    let right = network(left, &masks, true);
    let mut out = [0; 20];
    out[..4].copy_from_slice(&right[..4]);
    out[4..].copy_from_slice(&digest32(left, right));
    out
}
fn gf(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0;
    while b != 0 {
        if b & 1 != 0 {
            p ^= a;
        }
        let high = a & 0x80;
        a <<= 1;
        if high != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    p
}
fn xor(state: &mut [u8; 16], key: &[u8; 16]) {
    for i in 0..16 {
        state[i] ^= key[i];
    }
}
fn shift(state: &mut [u8; 16], inverse: bool) {
    let previous = *state;
    for row in 0..4 {
        for col in 0..4 {
            let from = if inverse {
                (col + 4 - row) & 3
            } else {
                (col + row) & 3
            };
            state[4 * col + row] = previous[4 * from + row];
        }
    }
}
fn mix(state: &mut [u8; 16], inverse: bool) {
    let coefficients = if inverse {
        [14, 11, 13, 9]
    } else {
        [2, 3, 1, 1]
    };
    for column in state.chunks_exact_mut(4) {
        let previous = column.to_vec();
        for i in 0..4 {
            column[i] = 0;
            for j in 0..4 {
                column[i] ^= gf(previous[j], coefficients[(j + 4 - i) & 3]);
            }
        }
    }
}
pub(crate) fn message_crypt(mode: usize, body: &[u8; 128], encrypt: bool) -> [u8; 128] {
    let mut out = [0; 128];
    let mut chain = MESSAGE_IV[mode];
    for n in 0..8 {
        let mut state: [u8; 16] = body[n * 16..n * 16 + 16].try_into().expect("fixed block");
        if encrypt {
            xor(&mut state, &chain);
            xor(&mut state, &ROUND0);
            for b in &mut state {
                *b = SBOX[*b as usize];
            }
            shift(&mut state, false);
            for r in 0..9 {
                mix(&mut state, false);
                xor(&mut state, &MIDDLE_KEYS[mode * 9 + r]);
                for b in &mut state {
                    *b = SBOX[*b as usize];
                }
                shift(&mut state, false);
            }
            xor(&mut state, &ROUND10);
            chain = state;
        } else {
            xor(&mut state, &ROUND10);
            for r in (0..9).rev() {
                shift(&mut state, true);
                for b in &mut state {
                    *b = INV_SBOX[*b as usize];
                }
                xor(&mut state, &MIDDLE_KEYS[mode * 9 + r]);
                mix(&mut state, true);
            }
            shift(&mut state, true);
            for b in &mut state {
                *b = INV_SBOX[*b as usize];
            }
            xor(&mut state, &ROUND0);
            xor(&mut state, &chain);
            chain.copy_from_slice(&body[n * 16..n * 16 + 16]);
        }
        out[n * 16..n * 16 + 16].copy_from_slice(&state);
    }
    out
}

#[doc(hidden)]
pub fn descriptor_vector(
    local: [u8; 128],
    mode: usize,
    body: [u8; 128],
) -> Option<([u8; 20], [u8; 20])> {
    if mode > 3 {
        return None;
    }
    let remote = message_crypt(mode, &body, false);
    let d = descriptor(&local, &remote);
    Some((d, exchange_seed(d)))
}

#[doc(hidden)]
pub fn table_digest() -> Vec<u8> {
    let mut bytes = INPUT_MASK.to_vec();
    for tables in [&FIRST, &SECOND] {
        for lookup in &tables[..144] {
            for v in 0..=255 {
                bytes.push(substitute(*lookup, v, false));
            }
        }
        for row in tables[144..160].chunks_exact(4) {
            for v in 0..=255 {
                for lookup in row {
                    bytes.push(substitute(*lookup, v, true));
                }
            }
        }
        for lookup in &tables[160..] {
            for v in 0..=255 {
                bytes.push(substitute(*lookup, v, false));
            }
        }
    }
    ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .to_vec()
}

/// Deterministic facts for comparison with the separately executed Go source.
#[doc(hidden)]
pub fn keywrap_vector() -> Result<(Vec<u8>, Vec<u8>), Error> {
    let mut fp = FairPlay {
        local: [0x41; 128],
        remote: [0; 128],
        m3: None,
        confirmed: false,
    };
    fp.local[0] = 0;
    fp.local[1] = 1;
    let mut m2 = record(2, 130);
    m2[12] = 2;
    m2[13] = 3;
    let m3 = fp.exchange(&m2)?;
    let mut m4 = record(4, 20);
    m4[12..].copy_from_slice(&m3[144..]);
    fp.confirm(&m4)?;
    let key = std::array::from_fn(|i| i as u8);
    Ok((m3, fp.wrap_mask(key, [0x5a; 16])?.to_vec()))
}
