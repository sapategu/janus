//! HomeKit-style AirPlay pairing (HAP): TLV8, SRP-6a (RFC 5054 3072-bit group,
//! SHA-512), PIN pair-setup M1–M6 and pair-verify V1–V4, plus the
//! HKDF-SHA512/ChaCha20-Poly1305 primitives of the encrypted channels.
//! Ported from Doubletake 5e2f1cb `pairing.go`, `event_channel.go` and
//! `mirror.go`. Unlike that reference, Janus verifies the receiver's long-term
//! Ed25519 signature in M6 and in every V2, and requires the SRP server proof.
use crate::{Error, fairplay::random, wire::Connection};
use num_bigint::BigUint;
use ring::{
    aead, agreement, digest, hkdf,
    rand::SystemRandom,
    signature::{self, Ed25519KeyPair, KeyPair},
};
use std::{collections::BTreeMap, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite};

const METHOD: u8 = 0x00;
const IDENTIFIER: u8 = 0x01;
const SALT: u8 = 0x02;
const PUBLIC_KEY: u8 = 0x03;
const PROOF: u8 = 0x04;
const ENCRYPTED: u8 = 0x05;
const STATE: u8 = 0x06;
const ERROR: u8 = 0x07;
const RETRY_DELAY: u8 = 0x08;
const SIGNATURE: u8 = 0x0a;
const ACL: u8 = 0x12;
const MAX_TLV: usize = 4096;
/// Observed: a receiver answered pair-pin-start after 6.5 s (code on screen).
const PIN_START: Duration = Duration::from_secs(30);
const PAIRING_STEP: Duration = Duration::from_secs(15);
const MAX_IDENTIFIER: usize = 64;
/// OPACK `{"com.apple.ScreenCapture": true}`; required by screen-capture pairing.
const SCREEN_CAPTURE_ACL: &[u8] = b"\xe1\x57com.apple.ScreenCapture\x01";
/// RFC 5054 3072-bit group, generator 5.
const N: &str = concat!(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DD",
    "EF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED",
    "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F",
    "83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B",
    "E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183995497CEA956AE515D2261898FA0510",
    "15728E5A8AAAC42DAD33170D04507A33A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7",
    "ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864D87602733EC86A64521F2B18177B200C",
    "BBE117577A615D6C770988C0BAD946E208E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF",
);
const G: u8 = 5;

/// X-Apple-HKP pairing type, selected by advertised capability. Receivers
/// built on the licensed AirPlay SDK (MFi feature 26 or 51) use type 3;
/// first-party receivers use the screen-capture system pairing type 5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Sdk,
    ScreenCapture,
}
impl Kind {
    pub(crate) fn for_features(features: u64) -> Self {
        if features & ((1 << 26) | (1 << 51)) != 0 {
            Self::Sdk
        } else {
            Self::ScreenCapture
        }
    }
    fn value(self) -> &'static str {
        match self {
            Self::Sdk => "3",
            Self::ScreenCapture => "5",
        }
    }
}

/// This sender's long-term pairing identity. Never printed.
pub(crate) struct Identity {
    pub id: String,
    pub seed: [u8; 32],
}
impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity").finish_non_exhaustive()
    }
}
impl Identity {
    pub(crate) fn generate() -> Result<Self, Error> {
        let mut b = random::<16>()?;
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        let hex = b.iter().map(|x| format!("{x:02X}")).collect::<String>();
        Ok(Self {
            id: format!(
                "{}-{}-{}-{}-{}",
                &hex[..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..]
            ),
            seed: random()?,
        })
    }
    fn key(&self) -> Result<Ed25519KeyPair, Error> {
        Ed25519KeyPair::from_seed_unchecked(&self.seed).map_err(|_| Error::Entropy)
    }
}

/// A receiver's pairing identifier and long-term Ed25519 public key.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Peer {
    pub id: Vec<u8>,
    pub key: [u8; 32],
}
impl std::fmt::Debug for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peer").finish_non_exhaustive()
    }
}

/// Ordered TLV8; values longer than 255 bytes are split into fragments.
pub(crate) fn tlv(items: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (tag, value) in items {
        if value.is_empty() {
            out.extend_from_slice(&[*tag, 0]);
        }
        for chunk in value.chunks(255) {
            out.push(*tag);
            out.push(chunk.len() as u8);
            out.extend_from_slice(chunk);
        }
    }
    out
}
/// Strict bounded TLV8: fragments must be adjacent; truncation is an error.
pub(crate) fn parse_tlv(bytes: &[u8]) -> Result<BTreeMap<u8, Vec<u8>>, Error> {
    if bytes.len() > MAX_TLV {
        return Err(Error::Protocol);
    }
    let mut out: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    let mut previous = None;
    let mut rest = bytes;
    while !rest.is_empty() {
        let [tag, len, tail @ ..] = rest else {
            return Err(Error::Protocol);
        };
        let len = usize::from(*len);
        if tail.len() < len {
            return Err(Error::Protocol);
        }
        let value = &tail[..len];
        match out.get_mut(tag) {
            Some(existing) if previous == Some(*tag) => existing.extend_from_slice(value),
            Some(_) => return Err(Error::Protocol),
            None => {
                out.insert(*tag, value.to_vec());
            }
        }
        previous = Some(*tag);
        rest = &tail[len..];
    }
    Ok(out)
}

fn sha512(parts: &[&[u8]]) -> [u8; 64] {
    let mut h = digest::Context::new(&digest::SHA512);
    for part in parts {
        h.update(part);
    }
    h.finish().as_ref().try_into().expect("SHA-512 size")
}
fn pad(value: &BigUint) -> Vec<u8> {
    let bytes = value.to_bytes_be();
    let mut out = vec![0; 384usize.saturating_sub(bytes.len())];
    out.extend(bytes);
    out
}
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Client side of one SRP-6a exchange.
pub(crate) struct Srp {
    /// A, natural big-endian bytes.
    pub public: Vec<u8>,
    /// M1 = H(H(N) xor H(g), H(I), s, A, B, K).
    pub proof: [u8; 64],
    /// K = H(S).
    pub session: [u8; 64],
    /// Expected M2 = H(A, M1, K).
    pub server_proof: [u8; 64],
}
impl std::fmt::Debug for Srp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Srp").finish_non_exhaustive()
    }
}
/// Username "Pair-Setup"; hashes and padding follow Doubletake's
/// `completeSRPExchange` (padded k and u, natural bytes in the proofs).
pub(crate) fn srp(
    pin: &[u8],
    salt: &[u8],
    server: &[u8],
    private: &[u8; 32],
) -> Result<Srp, Error> {
    if salt.is_empty() || salt.len() > 64 || server.is_empty() || server.len() > 384 {
        return Err(Error::Authentication);
    }
    let n = BigUint::parse_bytes(N.as_bytes(), 16).expect("constant group");
    let g = BigUint::from(G);
    let zero = BigUint::default();
    let b = BigUint::from_bytes_be(server);
    if b >= n || b == zero {
        return Err(Error::Authentication);
    }
    let x = BigUint::from_bytes_be(&sha512(&[salt, &sha512(&[b"Pair-Setup:", pin])]));
    let k = BigUint::from_bytes_be(&sha512(&[&pad(&n), &pad(&g)]));
    let mut a = BigUint::from_bytes_be(private);
    if a == zero {
        a = BigUint::from(1u8);
    }
    let big_a = g.modpow(&a, &n);
    let u = BigUint::from_bytes_be(&sha512(&[&pad(&big_a), &pad(&b)]));
    if u == zero {
        return Err(Error::Authentication);
    }
    let kgx = (&k * g.modpow(&x, &n)) % &n;
    let base = (&b + &n - kgx) % &n;
    let s = base.modpow(&(&a + &u * &x), &n);
    let session = sha512(&[&s.to_bytes_be()]);
    let hn = sha512(&[&n.to_bytes_be()]);
    let hg = sha512(&[&g.to_bytes_be()]);
    let mut xor = [0; 64];
    for (i, v) in xor.iter_mut().enumerate() {
        *v = hn[i] ^ hg[i];
    }
    let public = big_a.to_bytes_be();
    let proof = sha512(&[
        &xor,
        &sha512(&[b"Pair-Setup"]),
        salt,
        &public,
        &b.to_bytes_be(),
        &session,
    ]);
    let server_proof = sha512(&[&public, &proof, &session]);
    Ok(Srp {
        public,
        proof,
        session,
        server_proof,
    })
}

struct Len(usize);
impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}
pub(crate) fn hkdf32(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA512, salt).extract(ikm);
    let info = [info];
    let okm = prk.expand(&info, Len(32)).expect("32-byte HKDF output");
    let mut out = [0; 32];
    okm.fill(&mut out).expect("32-byte HKDF output");
    out
}

/// ChaCha20-Poly1305 (IETF) with explicit nonces.
pub(crate) struct Aead(aead::LessSafeKey);
impl Aead {
    pub(crate) fn new(key: &[u8; 32]) -> Self {
        Self(aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key).expect("32-byte key"),
        ))
    }
    pub(crate) fn seal(&self, nonce: [u8; 12], aad: &[u8], data: &mut Vec<u8>) {
        self.0
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad),
                data,
            )
            .expect("bounded plaintext");
    }
    pub(crate) fn open(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        data: &mut Vec<u8>,
    ) -> Result<(), Error> {
        let len = self
            .0
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad),
                data,
            )
            .map_err(|_| Error::Authentication)?
            .len();
        data.truncate(len);
        Ok(())
    }
}
/// Channel nonce: 32 zero bits then a 64-bit little-endian counter.
pub(crate) fn counter(n: u64) -> [u8; 12] {
    let mut out = [0; 12];
    out[4..].copy_from_slice(&n.to_le_bytes());
    out
}
fn label(text: &[u8; 8]) -> [u8; 12] {
    let mut out = [0; 12];
    out[4..].copy_from_slice(text);
    out
}

/// Directional keys of one encrypted channel.
pub(crate) struct ChannelKeys {
    pub read: [u8; 32],
    pub write: [u8; 32],
}
pub(crate) fn control_keys(shared: &[u8]) -> ChannelKeys {
    ChannelKeys {
        read: hkdf32(shared, b"Control-Salt", b"Control-Read-Encryption-Key"),
        write: hkdf32(shared, b"Control-Salt", b"Control-Write-Encryption-Key"),
    }
}
/// The event channel reverses the key direction: the receiver writes with
/// Events-Write and the sender answers with Events-Read.
pub(crate) fn event_keys(shared: &[u8]) -> ChannelKeys {
    ChannelKeys {
        read: hkdf32(shared, b"Events-Salt", b"Events-Write-Encryption-Key"),
        write: hkdf32(shared, b"Events-Salt", b"Events-Read-Encryption-Key"),
    }
}
/// Screen data key for one video stream connection.
pub(crate) fn video_key(shared: &[u8], stream: u64) -> [u8; 32] {
    hkdf32(
        shared,
        format!("DataStream-Salt{stream}").as_bytes(),
        b"DataStream-Output-Encryption-Key",
    )
}

fn headers(kind: Kind, identity: &Identity, verify: bool) -> Vec<(&'static str, String)> {
    let mut out = vec![("X-Apple-HKP", kind.value().to_owned())];
    if kind == Kind::ScreenCapture {
        out.push(("X-Apple-Client-Name", janus_media::identity::sender_name()));
        out.push(("X-Apple-Client-ID", identity.id.clone()));
        if verify {
            out.push(("X-Apple-PD", "1".to_owned()));
        }
    }
    out
}
fn refs<'a>(headers: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    headers.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// Asks the receiver to show its one-time code. HTTP 453 is an accepted
/// asynchronous start.
pub(crate) async fn start_pin<S: AsyncRead + AsyncWrite + Unpin>(
    c: &mut Connection<S>,
    kind: Kind,
    identity: &Identity,
) -> Result<(), Error> {
    let mut h = headers(kind, identity, false);
    if kind == Kind::ScreenCapture {
        h.push(("X-Apple-SupportedPINLengths", "4".to_owned()));
    }
    match c
        .request_within("POST", "/pair-pin-start", "", &[], &refs(&h), PIN_START)
        .await
    {
        Ok(_) | Err(Error::Status(453)) => Ok(()),
        Err(e) => Err(e.response("pair-pin-start", "malformed RTSP headers or framing")),
    }
}

/// One pairing exchange: send TLV8, require the next state or a TLV error.
/// Only M1 honours a bounded receiver backoff (≤30 s, ≤3 times).
async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    c: &mut Connection<S>,
    path: &str,
    body: &[u8],
    headers: &[(&'static str, String)],
    state: u8,
    stage: &'static str,
) -> Result<BTreeMap<u8, Vec<u8>>, Error> {
    for retry in 0.. {
        let response = c
            .request_within(
                "POST",
                path,
                "application/octet-stream",
                body,
                &refs(headers),
                PAIRING_STEP,
            )
            .await
            .map_err(|e| e.response(stage, "malformed RTSP headers or framing"))?;
        if response.body.is_empty() && state == 4 && path == "/pair-verify" {
            return Ok(BTreeMap::new());
        }
        let m = parse_tlv(&response.body).map_err(|e| e.response(stage, "malformed TLV8"))?;
        if let Some(code) = m.get(&ERROR) {
            match code.first() {
                Some(3) if state == 2 && retry < 3 => {
                    let delay = m
                        .get(&RETRY_DELAY)
                        .filter(|d| !d.is_empty() && d.len() <= 8)
                        .map(|d| {
                            d.iter()
                                .enumerate()
                                .fold(0u64, |acc, (i, b)| acc | (u64::from(*b) << (8 * i)))
                        })
                        .unwrap_or(0);
                    if delay > 30 {
                        return Err(Error::PairingRejected(3));
                    }
                    tokio::time::sleep(Duration::from_secs(delay)).await;
                    continue;
                }
                Some(2) => return Err(Error::PinRejected),
                Some(code) => return Err(Error::PairingRejected(*code)),
                None => return Err(Error::Protocol.response(stage, "empty TLV error")),
            }
        }
        if m.get(&STATE).map(Vec::as_slice) != Some(&[state]) {
            return Err(Error::InvalidResponse {
                stage,
                reason: "unexpected pairing state",
            });
        }
        return Ok(m);
    }
    unreachable!("bounded retry loop returns")
}
/// PIN pair-setup M1–M6. Returns the receiver's verified long-term identity.
pub(crate) async fn setup<S: AsyncRead + AsyncWrite + Unpin>(
    c: &mut Connection<S>,
    kind: Kind,
    identity: &Identity,
    pin: &str,
) -> Result<Peer, Error> {
    if pin.is_empty() || pin.len() > 64 || !pin.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Error::PinRejected);
    }
    let h = headers(kind, identity, false);
    let m1 = tlv(&[(METHOD, &[0]), (STATE, &[1])]);
    let m2 = exchange(c, "/pair-setup", &m1, &h, 2, "pair-setup M2").await?;
    let (Some(salt), Some(server)) = (m2.get(&SALT), m2.get(&PUBLIC_KEY)) else {
        return Err(Error::InvalidResponse {
            stage: "pair-setup M2",
            reason: "missing salt or public key",
        });
    };
    let srp = srp(pin.as_bytes(), salt, server, &random()?)?;
    let mut public = vec![0; 384usize.saturating_sub(srp.public.len())];
    public.extend_from_slice(&srp.public);
    let m3 = tlv(&[(STATE, &[3]), (PUBLIC_KEY, &public), (PROOF, &srp.proof)]);
    let m4 = exchange(c, "/pair-setup", &m3, &h, 4, "pair-setup M4").await?;
    if !m4.get(&PROOF).is_some_and(|p| same(p, &srp.server_proof)) {
        return Err(Error::Authentication);
    }
    let encrypt = Aead::new(&hkdf32(
        &srp.session,
        b"Pair-Setup-Encrypt-Salt",
        b"Pair-Setup-Encrypt-Info",
    ));
    let controller = hkdf32(
        &srp.session,
        b"Pair-Setup-Controller-Sign-Salt",
        b"Pair-Setup-Controller-Sign-Info",
    );
    let key = identity.key()?;
    let own = key.public_key().as_ref();
    let signature = key.sign(&[&controller[..], identity.id.as_bytes(), own].concat());
    let mut items: Vec<(u8, &[u8])> = vec![
        (IDENTIFIER, identity.id.as_bytes()),
        (PUBLIC_KEY, own),
        (SIGNATURE, signature.as_ref()),
    ];
    if kind == Kind::ScreenCapture {
        items.push((ACL, SCREEN_CAPTURE_ACL));
    }
    let mut sealed = tlv(&items);
    encrypt.seal(label(b"PS-Msg05"), &[], &mut sealed);
    let m5 = tlv(&[(ENCRYPTED, &sealed), (STATE, &[5])]);
    let m6 = exchange(c, "/pair-setup", &m5, &h, 6, "pair-setup M6").await?;
    let mut data = m6.get(&ENCRYPTED).cloned().ok_or(Error::Authentication)?;
    encrypt.open(label(b"PS-Msg06"), &[], &mut data)?;
    let sub = parse_tlv(&data).map_err(|_| Error::Authentication)?;
    let (Some(id), Some(ltpk), Some(sig)) = (
        sub.get(&IDENTIFIER),
        sub.get(&PUBLIC_KEY),
        sub.get(&SIGNATURE),
    ) else {
        return Err(Error::Authentication);
    };
    if id.is_empty() || id.len() > MAX_IDENTIFIER || ltpk.len() != 32 || sig.len() != 64 {
        return Err(Error::Authentication);
    }
    let accessory = hkdf32(
        &srp.session,
        b"Pair-Setup-Accessory-Sign-Salt",
        b"Pair-Setup-Accessory-Sign-Info",
    );
    signature::UnparsedPublicKey::new(&signature::ED25519, ltpk)
        .verify(&[&accessory[..], id, ltpk].concat(), sig)
        .map_err(|_| Error::Authentication)?;
    Ok(Peer {
        id: id.clone(),
        key: ltpk.as_slice().try_into().expect("checked length"),
    })
}

/// Pair-verify V1–V4 against a stored peer. Returns the X25519 shared secret
/// that keys the control, event and screen-data channels.
pub(crate) async fn verify<S: AsyncRead + AsyncWrite + Unpin>(
    c: &mut Connection<S>,
    kind: Kind,
    identity: &Identity,
    peer: &Peer,
) -> Result<[u8; 32], Error> {
    let private =
        agreement::EphemeralPrivateKey::generate(&agreement::X25519, &SystemRandom::new())
            .map_err(|_| Error::Entropy)?;
    let public = private.compute_public_key().map_err(|_| Error::Entropy)?;
    let h = headers(kind, identity, true);
    let v1 = tlv(&[(STATE, &[1]), (PUBLIC_KEY, public.as_ref())]);
    let v2 = exchange(c, "/pair-verify", &v1, &h, 2, "pair-verify V2").await?;
    let remote = v2
        .get(&PUBLIC_KEY)
        .filter(|k| k.len() == 32)
        .cloned()
        .ok_or(Error::Authentication)?;
    let shared: [u8; 32] = agreement::agree_ephemeral(
        private,
        &agreement::UnparsedPublicKey::new(&agreement::X25519, &remote),
        |k| k.try_into().expect("X25519 size"),
    )
    .map_err(|_| Error::Authentication)?;
    let cipher = Aead::new(&hkdf32(
        &shared,
        b"Pair-Verify-Encrypt-Salt",
        b"Pair-Verify-Encrypt-Info",
    ));
    let mut data = v2.get(&ENCRYPTED).cloned().ok_or(Error::Authentication)?;
    cipher.open(label(b"PV-Msg02"), &[], &mut data)?;
    let sub = parse_tlv(&data).map_err(|_| Error::Authentication)?;
    let (Some(id), Some(sig)) = (sub.get(&IDENTIFIER), sub.get(&SIGNATURE)) else {
        return Err(Error::Authentication);
    };
    if !same(id, &peer.id) {
        return Err(Error::Authentication);
    }
    signature::UnparsedPublicKey::new(&signature::ED25519, &peer.key)
        .verify(&[&remote[..], id, public.as_ref()].concat(), sig)
        .map_err(|_| Error::Authentication)?;
    let key = identity.key()?;
    let proof = key.sign(&[public.as_ref(), identity.id.as_bytes(), &remote[..]].concat());
    let mut sealed = tlv(&[
        (IDENTIFIER, identity.id.as_bytes()),
        (SIGNATURE, proof.as_ref()),
    ]);
    cipher.seal(label(b"PV-Msg03"), &[], &mut sealed);
    let v3 = tlv(&[(STATE, &[3]), (ENCRYPTED, &sealed)]);
    exchange(c, "/pair-verify", &v3, &h, 4, "pair-verify V4").await?;
    Ok(shared)
}
