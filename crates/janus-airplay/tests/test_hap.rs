//! HomeKit-style AirPlay pairing, encrypted channels and paired video
//! framing. The scripted receiver below implements the accessory side
//! independently (ring + num-bigint), so the client is not checked only
//! against its own code.
mod support;
use janus_airplay::{Error, Profile, testing};
use num_bigint::BigUint;
use ring::{
    aead, agreement, digest, hkdf,
    rand::SystemRandom,
    signature::{self, Ed25519KeyPair, KeyPair},
};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

const N_HEX: &str = concat!(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DD",
    "EF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED",
    "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F",
    "83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B",
    "E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183995497CEA956AE515D2261898FA0510",
    "15728E5A8AAAC42DAD33170D04507A33A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7",
    "ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864D87602733EC86A64521F2B18177B200C",
    "BBE117577A615D6C770988C0BAD946E208E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF",
);
const ACCESSORY_ID: &[u8] = b"AA:BB:CC:DD:EE:FF";

fn h(parts: &[&[u8]]) -> Vec<u8> {
    let mut c = digest::Context::new(&digest::SHA512);
    for p in parts {
        c.update(p);
    }
    c.finish().as_ref().to_vec()
}
fn pad(n: &BigUint) -> Vec<u8> {
    let b = n.to_bytes_be();
    let mut out = vec![0; 384 - b.len()];
    out.extend(b);
    out
}
struct L(usize);
impl hkdf::KeyType for L {
    fn len(&self) -> usize {
        self.0
    }
}
fn kdf(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let info = [info];
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA512, salt).extract(ikm);
    let okm = prk.expand(&info, L(32)).unwrap();
    let mut out = [0; 32];
    okm.fill(&mut out).unwrap();
    out
}
fn key(k: &[u8; 32]) -> aead::LessSafeKey {
    aead::LessSafeKey::new(aead::UnboundKey::new(&aead::CHACHA20_POLY1305, k).unwrap())
}
fn nonce_label(l: &[u8; 8]) -> aead::Nonce {
    let mut n = [0; 12];
    n[4..].copy_from_slice(l);
    aead::Nonce::assume_unique_for_key(n)
}
fn nonce_count(c: u64) -> aead::Nonce {
    let mut n = [0; 12];
    n[4..].copy_from_slice(&c.to_le_bytes());
    aead::Nonce::assume_unique_for_key(n)
}
fn seal(k: &[u8; 32], n: aead::Nonce, aad: &[u8], plain: &[u8]) -> Vec<u8> {
    let mut d = plain.to_vec();
    key(k)
        .seal_in_place_append_tag(n, aead::Aad::from(aad), &mut d)
        .unwrap();
    d
}
fn open(k: &[u8; 32], n: aead::Nonce, aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    let mut d = sealed.to_vec();
    let len = key(k)
        .open_in_place(n, aead::Aad::from(aad), &mut d)
        .ok()?
        .len();
    d.truncate(len);
    Some(d)
}
fn tlv_get(m: &std::collections::BTreeMap<u8, Vec<u8>>, tag: u8) -> Vec<u8> {
    m.get(&tag).cloned().unwrap_or_default()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Accessory {
    Good,
    BadSetupSignature,
    BadVerifySignature,
}
#[derive(Default)]
struct Seen {
    shared: Option<[u8; 32]>,
    controller_key: Vec<u8>,
    headers_hkp: Vec<String>,
    m5_had_acl: bool,
}

/// Accessory side of PIN pair-setup and pair-verify (HAP R2 §5.6/5.7 shapes,
/// as used by AirPlay receivers).
async fn accessory(
    mut s: DuplexStream,
    pin: &'static str,
    mode: Accessory,
    seen: Arc<Mutex<Seen>>,
) {
    let n = BigUint::parse_bytes(N_HEX.as_bytes(), 16).unwrap();
    let g = BigUint::from(5u8);
    let ltsk = Ed25519KeyPair::from_seed_unchecked(&[0x44; 32]).unwrap();
    let other = Ed25519KeyPair::from_seed_unchecked(&[0x45; 32]).unwrap();
    let salt = [7u8; 16];
    let b = BigUint::from_bytes_be(&[0x5a; 32]);
    let x = BigUint::from_bytes_be(&h(&[&salt, &h(&[b"Pair-Setup:", pin.as_bytes()])]));
    let v = g.modpow(&x, &n);
    let k = BigUint::from_bytes_be(&h(&[&pad(&n), &pad(&g)]));
    let big_b = (&k * &v + g.modpow(&b, &n)) % &n;
    let mut session_k = Vec::new();
    let mut controller = (Vec::new(), Vec::new());
    let mut verify = None::<([u8; 32], Vec<u8>, Vec<u8>)>;
    while let Some((method, path, seq, body, headers)) = request_with_headers(&mut s).await {
        assert_eq!(method, "POST");
        if let Some(hkp) = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("X-Apple-HKP"))
        {
            seen.lock().unwrap().headers_hkp.push(hkp.1.clone());
        }
        let m = testing::parse_tlv(&body).unwrap_or_default();
        let state = tlv_get(&m, 6).first().copied().unwrap_or(0);
        let reply = match (path.as_str(), state) {
            ("/pair-pin-start", _) => Vec::new(),
            ("/pair-setup", 1) => testing::tlv(&[(6, &[2]), (2, &salt), (3, &big_b.to_bytes_be())]),
            ("/pair-setup", 3) => {
                let a = BigUint::from_bytes_be(&tlv_get(&m, 3));
                let u = BigUint::from_bytes_be(&h(&[&pad(&a), &pad(&big_b)]));
                let premaster = ((&a * v.modpow(&u, &n)) % &n).modpow(&b, &n);
                session_k = h(&[&premaster.to_bytes_be()]);
                let hx: Vec<u8> = h(&[&n.to_bytes_be()])
                    .iter()
                    .zip(h(&[&g.to_bytes_be()]))
                    .map(|(p, q)| p ^ q)
                    .collect();
                let m1 = h(&[
                    &hx,
                    &h(&[b"Pair-Setup"]),
                    &salt,
                    &a.to_bytes_be(),
                    &big_b.to_bytes_be(),
                    &session_k,
                ]);
                if m1 != tlv_get(&m, 4) {
                    testing::tlv(&[(6, &[4]), (7, &[2])])
                } else {
                    testing::tlv(&[(6, &[4]), (4, &h(&[&a.to_bytes_be(), &m1, &session_k]))])
                }
            }
            ("/pair-setup", 5) => {
                let enc = kdf(
                    &session_k,
                    b"Pair-Setup-Encrypt-Salt",
                    b"Pair-Setup-Encrypt-Info",
                );
                let sub = open(&enc, nonce_label(b"PS-Msg05"), &[], &tlv_get(&m, 5)).unwrap();
                let sub = testing::parse_tlv(&sub).unwrap();
                let (id, ltpk, sig) = (tlv_get(&sub, 1), tlv_get(&sub, 3), tlv_get(&sub, 10));
                let ctrl = kdf(
                    &session_k,
                    b"Pair-Setup-Controller-Sign-Salt",
                    b"Pair-Setup-Controller-Sign-Info",
                );
                signature::UnparsedPublicKey::new(&signature::ED25519, &ltpk)
                    .verify(&[&ctrl[..], &id, &ltpk].concat(), &sig)
                    .expect("controller signature");
                {
                    let mut seen = seen.lock().unwrap();
                    seen.controller_key = ltpk.clone();
                    seen.m5_had_acl = sub.contains_key(&0x12);
                }
                controller = (id, ltpk);
                let acc = kdf(
                    &session_k,
                    b"Pair-Setup-Accessory-Sign-Salt",
                    b"Pair-Setup-Accessory-Sign-Info",
                );
                let public = ltsk.public_key().as_ref();
                let mut sig = ltsk
                    .sign(&[&acc[..], ACCESSORY_ID, public].concat())
                    .as_ref()
                    .to_vec();
                if mode == Accessory::BadSetupSignature {
                    sig[0] ^= 1;
                }
                let sub = testing::tlv(&[(1, ACCESSORY_ID), (3, public), (10, &sig)]);
                testing::tlv(&[
                    (6, &[6]),
                    (5, &seal(&enc, nonce_label(b"PS-Msg06"), &[], &sub)),
                ])
            }
            ("/pair-verify", 1) => {
                let client = tlv_get(&m, 3);
                let private = agreement::EphemeralPrivateKey::generate(
                    &agreement::X25519,
                    &SystemRandom::new(),
                )
                .unwrap();
                let public = private.compute_public_key().unwrap().as_ref().to_vec();
                let shared: [u8; 32] = agreement::agree_ephemeral(
                    private,
                    &agreement::UnparsedPublicKey::new(&agreement::X25519, &client),
                    |k| k.try_into().unwrap(),
                )
                .unwrap();
                let signer = if mode == Accessory::BadVerifySignature {
                    &other
                } else {
                    &ltsk
                };
                let sig = signer.sign(&[&public[..], ACCESSORY_ID, &client].concat());
                let enc = kdf(
                    &shared,
                    b"Pair-Verify-Encrypt-Salt",
                    b"Pair-Verify-Encrypt-Info",
                );
                let sub = testing::tlv(&[(1, ACCESSORY_ID), (10, sig.as_ref())]);
                verify = Some((shared, public.clone(), client));
                testing::tlv(&[
                    (6, &[2]),
                    (3, &public),
                    (5, &seal(&enc, nonce_label(b"PV-Msg02"), &[], &sub)),
                ])
            }
            ("/pair-verify", 3) => {
                let (shared, public, client) = verify.clone().unwrap();
                let enc = kdf(
                    &shared,
                    b"Pair-Verify-Encrypt-Salt",
                    b"Pair-Verify-Encrypt-Info",
                );
                let sub = open(&enc, nonce_label(b"PV-Msg03"), &[], &tlv_get(&m, 5)).unwrap();
                let sub = testing::parse_tlv(&sub).unwrap();
                assert_eq!(tlv_get(&sub, 1), controller.0);
                signature::UnparsedPublicKey::new(&signature::ED25519, &controller.1)
                    .verify(
                        &[&client[..], &controller.0, &public].concat(),
                        &tlv_get(&sub, 10),
                    )
                    .expect("controller verify signature");
                seen.lock().unwrap().shared = Some(shared);
                testing::tlv(&[(6, &[4])])
            }
            other => panic!("unexpected request {other:?}"),
        };
        support::respond(&mut s, seq, 200, &reply).await;
    }
}
type Request = (String, String, u32, Vec<u8>, Vec<(String, String)>);
async fn request_with_headers(s: &mut DuplexStream) -> Option<Request> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(s.read_u8().await.ok()?);
    }
    let text = String::from_utf8(head).unwrap();
    let mut lines = text.split("\r\n");
    let first: Vec<_> = lines.next().unwrap().split_whitespace().collect();
    let mut headers = Vec::new();
    let (mut len, mut seq) = (0, 0);
    for line in lines.filter(|l| !l.is_empty()) {
        let (k, v) = line.split_once(':').unwrap();
        let v = v.trim().to_owned();
        if k.eq_ignore_ascii_case("Content-Length") {
            len = v.parse().unwrap();
        }
        if k.eq_ignore_ascii_case("CSeq") {
            seq = v.parse().unwrap();
        }
        headers.push((k.to_owned(), v));
    }
    let mut body = vec![0; len];
    s.read_exact(&mut body).await.ok()?;
    Some((first[0].into(), first[1].into(), seq, body, headers))
}

async fn pair(
    mode: Accessory,
    pin_on_tv: &'static str,
    typed: &str,
    screen_capture: bool,
) -> (
    Result<(String, Vec<u8>, Vec<u8>, [u8; 32], [u8; 32]), Error>,
    Arc<Mutex<Seen>>,
) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let seen = Arc::new(Mutex::new(Seen::default()));
    let task = tokio::spawn(accessory(server, pin_on_tv, mode, seen.clone()));
    let result = testing::pin_pairing(client, screen_capture, typed).await;
    drop(task);
    (result, seen)
}

#[test]
fn test_srp_matches_independent_two_sided_vector() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/srp_vector.txt"
    ))
    .unwrap();
    let get = |name: &str| -> Vec<u8> {
        let line = text
            .lines()
            .find(|l| l.starts_with(&format!("{name}=")))
            .unwrap();
        (0..line.len() - name.len() - 1)
            .step_by(2)
            .map(|i| u8::from_str_radix(&line[name.len() + 1 + i..name.len() + 3 + i], 16).unwrap())
            .collect()
    };
    let salt: Vec<u8> = (1..=16).collect();
    let (a, m1, k, m2) = testing::srp_vector(b"3939", &salt, &get("B"), &[0xa5; 32]).unwrap();
    assert_eq!(a, get("A"));
    assert_eq!(m1.to_vec(), get("M1"));
    assert_eq!(k.to_vec(), get("K"));
    assert_eq!(m2.to_vec(), get("M2"));
    // A server value of 0 or ≥ N is rejected before any proof is computed.
    assert_eq!(
        testing::srp_vector(b"3939", &salt, &[0], &[0xa5; 32]).err(),
        Some(Error::Authentication)
    );
    assert_eq!(
        testing::srp_vector(b"3939", &salt, &[0xff; 384], &[0xa5; 32]).err(),
        Some(Error::Authentication)
    );
}

#[test]
fn test_tlv_fragments_long_values_and_rejects_truncation() {
    let long = vec![9u8; 600];
    let bytes = testing::tlv(&[(6, &[1]), (3, &long), (1, &[])]);
    assert_eq!(bytes.len(), 3 + (2 + 255) * 2 + (2 + 90) + 2);
    let parsed = testing::parse_tlv(&bytes).unwrap();
    assert_eq!(parsed[&3], long);
    assert_eq!(parsed[&1], Vec::<u8>::new());
    assert!(testing::parse_tlv(&bytes[..bytes.len() - 1]).is_err());
    // A tag that reappears after a different tag is not a fragment.
    assert!(testing::parse_tlv(&[3, 1, 0, 6, 1, 1, 3, 1, 0]).is_err());
    assert!(testing::parse_tlv(&vec![0; 4097]).is_err());
}

#[tokio::test]
async fn test_pin_pairing_verifies_receiver_identity_and_agrees_on_secret() {
    let (result, seen) = pair(Accessory::Good, "9386", "9386", false).await;
    let (_, sender_key, peer_id, peer_key, shared) = result.unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(peer_id, ACCESSORY_ID);
    assert_eq!(
        peer_key.as_slice(),
        Ed25519KeyPair::from_seed_unchecked(&[0x44; 32])
            .unwrap()
            .public_key()
            .as_ref()
    );
    assert_eq!(seen.controller_key, sender_key);
    assert_eq!(seen.shared, Some(shared));
    // SDK receivers (feature 26/51) use HKP type 3 on every request, no ACL.
    assert_eq!(seen.headers_hkp.len(), 6);
    assert!(seen.headers_hkp.iter().all(|v| v == "3"));
    assert!(!seen.m5_had_acl);
}

#[tokio::test]
async fn test_screen_capture_pairing_sends_type_5_and_access_request() {
    let (result, seen) = pair(Accessory::Good, "0068", "0068", true).await;
    assert!(result.is_ok());
    let seen = seen.lock().unwrap();
    assert!(seen.headers_hkp.iter().all(|v| v == "5"));
    assert!(seen.m5_had_acl);
}

#[tokio::test]
async fn test_wrong_pin_is_reported_as_rejected_code() {
    let (result, _) = pair(Accessory::Good, "9386", "1111", false).await;
    assert_eq!(result.err(), Some(Error::PinRejected));
}

#[tokio::test]
async fn test_forged_receiver_signatures_fail_pairing() {
    let (result, seen) = pair(Accessory::BadSetupSignature, "9386", "9386", false).await;
    assert_eq!(result.err(), Some(Error::Authentication));
    assert_eq!(seen.lock().unwrap().shared, None);
    let (result, seen) = pair(Accessory::BadVerifySignature, "9386", "9386", false).await;
    assert_eq!(result.err(), Some(Error::Authentication));
    // V3 is never sent after a bad V2 signature.
    assert_eq!(seen.lock().unwrap().shared, None);
}

#[tokio::test]
async fn test_pair_verify_rejects_a_different_stored_receiver_key() {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let seen = Arc::new(Mutex::new(Seen::default()));
    let _task = tokio::spawn(async move {
        // Minimal accessory: only answers V1 with a valid-looking V2.
        accessory(server, "0000", Accessory::Good, seen).await
    });
    let wrong = Ed25519KeyPair::from_seed_unchecked(&[0x99; 32]).unwrap();
    let result = testing::pair_verify(
        client,
        [1; 32],
        "SENDER",
        ACCESSORY_ID,
        wrong.public_key().as_ref().try_into().unwrap(),
    )
    .await;
    assert_eq!(result.err(), Some(Error::Authentication));
}

#[tokio::test]
async fn test_encrypted_channel_frames_match_hap_layout() {
    let keys = testing::channel_keys(&[3; 32]);
    let (control_read, control_write) = (keys[0], keys[1]);
    let (client, mut peer) = tokio::io::duplex(64 * 1024);
    let message = vec![b'x'; 2500];
    let reply = b"RTSP/1.0 200 OK\r\n\r\n".to_vec();
    let expected = reply.clone();
    let task = tokio::spawn(async move {
        testing::secure_exchange(
            client,
            control_read,
            control_write,
            &message,
            expected.len(),
        )
        .await
    });
    // Independent decoder: three frames of 1024/1024/452 with counter nonces.
    let mut plain = Vec::new();
    for (i, size) in [1024usize, 1024, 452].into_iter().enumerate() {
        let mut len = [0; 2];
        peer.read_exact(&mut len).await.unwrap();
        assert_eq!(u16::from_le_bytes(len) as usize, size);
        let mut sealed = vec![0; size + 16];
        peer.read_exact(&mut sealed).await.unwrap();
        plain.extend(open(&control_write, nonce_count(i as u64), &len, &sealed).unwrap());
    }
    assert_eq!(plain, vec![b'x'; 2500]);
    let len = (reply.len() as u16).to_le_bytes();
    let mut frame = len.to_vec();
    frame.extend(seal(&control_read, nonce_count(0), &len, &reply));
    peer.write_all(&frame).await.unwrap();
    assert_eq!(task.await.unwrap().unwrap(), reply);
}

#[tokio::test]
async fn test_encrypted_channel_rejects_tampered_frame() {
    let keys = testing::channel_keys(&[3; 32]);
    let (client, mut peer) = tokio::io::duplex(4096);
    let task =
        tokio::spawn(
            async move { testing::secure_exchange(client, keys[0], keys[1], b"hi", 4).await },
        );
    let mut first = [0; 2 + 2 + 16];
    peer.read_exact(&mut first).await.unwrap();
    let len = 4u16.to_le_bytes();
    let mut frame = len.to_vec();
    frame.extend(seal(&keys[0], nonce_count(0), &len, b"oops"));
    frame[3] ^= 0x80;
    peer.write_all(&frame).await.unwrap();
    assert!(matches!(task.await.unwrap(), Err(Error::Io(_))));
}

#[test]
fn test_channel_keys_follow_reference_directions() {
    let shared = [5u8; 32];
    let k = testing::channel_keys(&shared);
    assert_eq!(
        k[0],
        kdf(&shared, b"Control-Salt", b"Control-Read-Encryption-Key")
    );
    assert_eq!(
        k[1],
        kdf(&shared, b"Control-Salt", b"Control-Write-Encryption-Key")
    );
    // Event channel is reversed: the sender reads with Events-Write.
    assert_eq!(
        k[2],
        kdf(&shared, b"Events-Salt", b"Events-Write-Encryption-Key")
    );
    assert_eq!(
        k[3],
        kdf(&shared, b"Events-Salt", b"Events-Read-Encryption-Key")
    );
    assert_eq!(
        k[4],
        kdf(
            &shared,
            b"DataStream-Salt1",
            b"DataStream-Output-Encryption-Key"
        )
    );
}

#[test]
fn test_paired_video_frames_decrypt_with_header_as_aad() {
    let video_key = [8u8; 32];
    let sps = [0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x1f, 0xda];
    let pps = [0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80];
    let idr = [0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21, 0x43];
    let unit = [&sps[..], &pps, &idr].concat();
    let packets = testing::paired_vector(video_key, &unit).unwrap();
    assert_eq!(packets.len(), 2);
    // Codec configuration stays clear.
    assert_eq!(packets[0][4], 1);
    assert_eq!(&packets[0][128..129], &[1]);
    // VCL: size counts the tag, IDR flag set, ChaCha20-Poly1305 nonce 0.
    let video = &packets[1];
    let header = &video[..128];
    assert_eq!((header[4], header[5]), (0, 0x10));
    let size = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
    assert_eq!(size, video.len() - 128);
    let plain = open(&video_key, nonce_count(0), header, &video[128..]).unwrap();
    assert_eq!(plain, [&[0, 0, 0, 5][..], &idr[4..]].concat());
    // A modified header no longer authenticates.
    let mut bad = header.to_vec();
    bad[8] ^= 1;
    assert!(open(&video_key, nonce_count(0), &bad, &video[128..]).is_none());
}

#[test]
fn test_event_requests_are_framed_and_bounded() {
    let one = b"POST /command RTSP/1.0\r\nCSeq: 7\r\nContent-Length: 3\r\n\r\nabc";
    assert_eq!(
        testing::event_request(one).unwrap(),
        Some((Some(7), one.len()))
    );
    // Observed on a paired receiver: POST /command without CSeq.
    let bare = b"POST /command RTSP/1.0\r\nContent-Length: 2\r\nContent-Type: application/x-apple-binary-plist\r\n\r\nab";
    assert_eq!(
        testing::event_request(bare).unwrap(),
        Some((None, bare.len()))
    );
    assert_eq!(testing::event_request(&one[..one.len() - 1]).unwrap(), None);
    assert_eq!(
        testing::event_request(b"POST /command RTSP/1.0\r\nCSe").unwrap(),
        None
    );
    assert!(testing::event_request(b"garbage\r\n\r\n").is_err());
    assert!(testing::event_request(b"POST /c RTSP/1.0\r\nno colon here\r\n\r\n").is_err());
    let huge = b"POST /c RTSP/1.0\r\nCSeq: 1\r\nContent-Length: 999999\r\n\r\n";
    assert!(testing::event_request(huge).is_err());
    assert!(testing::event_request(&vec![b'a'; 17 * 1024]).is_err());
}

#[test]
fn test_pairing_store_round_trips_owner_only() {
    let dir = std::env::temp_dir().join(format!("janus-pairing-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("janus").join("airplay-pairings.plist");
    let (id, receivers) =
        testing::store_roundtrip(&path, "D7:00:00:00:00:01", ACCESSORY_ID, [4; 32]).unwrap();
    assert_eq!(
        receivers,
        vec![("D7:00:00:00:00:01".into(), ACCESSORY_ID.to_vec(), [4; 32])]
    );
    // A second save keeps the same sender identity.
    let (again, receivers) =
        testing::store_roundtrip(&path, "D7:00:00:00:00:02", ACCESSORY_ID, [5; 32]).unwrap();
    assert_eq!(again, id);
    assert_eq!(receivers.len(), 2);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    std::fs::write(&path, b"not a plist").unwrap();
    assert!(testing::store_roundtrip(&path, "x", ACCESSORY_ID, [4; 32]).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

fn info(features: u64, flags: u64, displays: plist::Value) -> Vec<u8> {
    support::bytes(support::dict([
        ("features", support::int(features)),
        ("statusFlags", support::int(flags)),
        ("deviceID", plist::Value::String("D7:00:00:00:00:01".into())),
        ("displays", displays),
    ]))
}
#[test]
fn test_capability_selects_paired_profile_by_features_only() {
    let display = |w: u64, h: u64, mw: u64, mh: u64| {
        plist::Value::Array(vec![support::dict([
            ("widthPixels", support::int(w)),
            ("heightPixels", support::int(h)),
            ("widthPixelsMax", support::int(mw)),
            ("heightPixelsMax", support::int(mh)),
        ])])
    };
    // Screen (7), CoreUtils pairing (43, 46, 48), MFi SDK (51), no raw pairing (27).
    let modern = (1 << 7) | (1 << 19) | (1 << 43) | (1 << 46) | (1 << 48) | (1 << 51);
    let c = testing::capability_from_info(&info(modern, 1 << 9, display(1920, 1080, 3840, 2160)))
        .unwrap();
    assert_eq!(c.profile, Profile::Paired);
    assert!(c.pairing_required && c.pin_pairing);
    // Screen audio is offered on the paired profile (ChaCha20-Poly1305 RTP).
    assert!(c.audio);
    assert_eq!((c.width, c.height), (3840, 2160));
    // A password receiver needs a credential Janus can't supply.
    let c = testing::capability_from_info(&info(modern, 1 << 7, display(1920, 1080, 3840, 2160)))
        .unwrap();
    assert!(c.pairing_required && !c.pin_pairing);
    // Raw pairing (27) keeps the legacy profile and its verified 1080p bound.
    let legacy = (1 << 7) | (1 << 27) | (1 << 46);
    let c =
        testing::capability_from_info(&info(legacy, 0, display(1920, 1080, 3840, 2160))).unwrap();
    assert_eq!(c.profile, Profile::Rtsp);
    assert!(!c.pairing_required && !c.pin_pairing);
    assert_eq!((c.width, c.height), (1920, 1080));
}

#[test]
fn test_paired_audio_packets_decrypt_with_timestamp_ssrc_aad() {
    let key = [9u8; 32];
    let samples: Vec<i16> = (0..janus_airplay::AUDIO_SAMPLES * 2)
        .map(|i| (i as i16).wrapping_mul(97))
        .collect();
    let packet = testing::paired_audio_packet(key, &samples, 0x0102_0304).unwrap();
    let alac = janus_airplay::alac(&samples).unwrap();
    // RTP ‖ ALAC sealed ‖ tag ‖ LE64 nonce, inside one unfragmented datagram.
    assert_eq!(packet.len(), 12 + alac.len() + 16 + 8);
    assert!(packet.len() <= 1472);
    assert_eq!(&packet[..2], &[0x80, 96]);
    assert_eq!(&packet[2..4], &7u16.to_be_bytes());
    assert_eq!(&packet[4..8], &0x0102_0304u32.to_be_bytes());
    assert_eq!(&packet[8..12], &0x1122_3344u32.to_be_bytes());
    let trailer = &packet[packet.len() - 8..];
    assert_eq!(trailer, &0u64.to_le_bytes());
    let sealed = &packet[12..packet.len() - 8];
    let plain = open(&key, nonce_count(0), &packet[4..12], sealed).unwrap();
    assert_eq!(plain, alac);
    // The sequence number is outside the AAD; timestamp/SSRC are inside.
    let mut other = packet[4..12].to_vec();
    other[0] ^= 1;
    assert!(open(&key, nonce_count(0), &other, sealed).is_none());
}
