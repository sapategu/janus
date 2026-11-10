#![allow(dead_code)]
use aes::{
    Aes128,
    cipher::{KeyIvInit, StreamCipher},
};
use ctr::Ctr128BE;
use plist::{Dictionary, Value};
use ring::{
    agreement, digest,
    rand::SystemRandom,
    signature::{self, Ed25519KeyPair, KeyPair},
};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    LegacyOrder,
    BadConfirmation,
    BadProof,
    RejectVideo,
    MalformedSetup,
    EmptyControl,
    EmptyVideo,
    EmptyAudio,
    StallVideo,
    Pin,
    NoAudio,
    SkipRecord,
}
pub type Log = Arc<Mutex<Vec<String>>>;
pub async fn request(s: &mut DuplexStream) -> Option<(String, String, u32, Vec<u8>)> {
    let mut h = Vec::new();
    while !h.ends_with(b"\r\n\r\n") {
        h.push(s.read_u8().await.ok()?);
        assert!(h.len() < 8192);
    }
    let h = String::from_utf8(h).unwrap();
    let mut lines = h.split("\r\n");
    let first = lines.next().unwrap().split_whitespace().collect::<Vec<_>>();
    let mut len = 0;
    let mut seq = 0;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("Content-Length") {
                len = v.trim().parse().unwrap();
            }
            if k.eq_ignore_ascii_case("CSeq") {
                seq = v.trim().parse().unwrap();
            }
        }
    }
    let mut bytes = vec![0; len];
    s.read_exact(&mut bytes).await.ok()?;
    Some((first[0].into(), first[1].into(), seq, bytes))
}
pub async fn respond(s: &mut DuplexStream, seq: u32, status: u16, body: &[u8]) {
    let h = format!(
        "RTSP/1.0 {status} Status\r\nCSeq: {seq}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    s.write_all(h.as_bytes()).await.unwrap();
    s.write_all(body).await.unwrap();
}
pub fn dict(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Dictionary(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}
pub fn int(n: u64) -> Value {
    Value::Integer(n.into())
}
pub fn bytes(value: Value) -> Vec<u8> {
    let mut out = Vec::new();
    value.to_writer_binary(&mut out).unwrap();
    out
}
pub fn decode(body: &[u8]) -> Dictionary {
    Value::from_reader(std::io::Cursor::new(body))
        .unwrap()
        .into_dictionary()
        .unwrap()
}
fn key(label: &str, secret: &[u8]) -> [u8; 16] {
    let mut data = label.as_bytes().to_vec();
    data.extend_from_slice(secret);
    digest::digest(&digest::SHA512, &data).as_ref()[..16]
        .try_into()
        .unwrap()
}
pub async fn receiver(mut s: DuplexStream, mode: Mode, log: Log) {
    let server = Ed25519KeyPair::from_seed_unchecked(&[0x35; 32]).unwrap();
    let mut client_identity = Vec::new();
    let mut proof_cipher = None;
    let mut signing = Vec::new();
    let mut last = 0;
    while let Some((method, path, seq, body)) = request(&mut s).await {
        assert!(seq > last);
        last = seq;
        log.lock().unwrap().push(format!(
            "{method} {}",
            if path.starts_with("rtsp:") {
                "session"
            } else {
                &path
            }
        ));
        let mut status = 200;
        let reply = match (method.as_str(), path.as_str()) {
            ("GET", "/info") => bytes(dict([
                (
                    "features",
                    int((1 << 7) | (1 << 19) | (1 << 27) | (1 << 28)),
                ),
                (
                    "statusFlags",
                    int(if mode == Mode::Pin { 1 << 9 } else { 0 }),
                ),
                (
                    "supportedFormats",
                    dict([(
                        "screenStream",
                        int(if mode == Mode::NoAudio { 0 } else { 0x40000 }),
                    )]),
                ),
            ])),
            ("POST", "/pair-setup") => {
                assert_eq!(body.len(), 32);
                client_identity = body;
                server.public_key().as_ref().to_vec()
            }
            ("POST", "/pair-verify") => {
                assert_eq!(body.len(), 68);
                if body[0] == 1 {
                    let private = agreement::EphemeralPrivateKey::generate(
                        &agreement::X25519,
                        &SystemRandom::new(),
                    )
                    .unwrap();
                    let public = private.compute_public_key().unwrap();
                    let shared = agreement::agree_ephemeral(
                        private,
                        &agreement::UnparsedPublicKey::new(&agreement::X25519, &body[4..36]),
                        |b| b.to_vec(),
                    )
                    .unwrap();
                    let mut cipher = Ctr128BE::<Aes128>::new(
                        &key("Pair-Verify-AES-Key", &shared).into(),
                        &key("Pair-Verify-AES-IV", &shared).into(),
                    );
                    let mut signed = public.as_ref().to_vec();
                    signed.extend_from_slice(&body[4..36]);
                    let mut proof = server.sign(&signed).as_ref().to_vec();
                    cipher.apply_keystream(&mut proof);
                    if mode == Mode::BadProof {
                        proof[0] ^= 1;
                    }
                    signing = body[4..36].to_vec();
                    signing.extend_from_slice(public.as_ref());
                    proof_cipher = Some(cipher);
                    let mut out = public.as_ref().to_vec();
                    out.extend(proof);
                    out
                } else {
                    let mut proof = body[4..].to_vec();
                    proof_cipher.as_mut().unwrap().apply_keystream(&mut proof);
                    signature::UnparsedPublicKey::new(&signature::ED25519, &client_identity)
                        .verify(&signing, &proof)
                        .unwrap();
                    Vec::new()
                }
            }
            ("POST", "/fp-setup") => {
                if body.len() == 16 {
                    assert_eq!(body, b"FPLY\x03\x01\x01\0\0\0\0\x04\x02\0\x03\xbb");
                    let mut out = b"FPLY\x03\x01\x02\0\0\0\0\x82\x02\x03".to_vec();
                    out.resize(142, 0);
                    out
                } else {
                    assert_eq!(body.len(), 164);
                    assert_eq!(body[..8], *b"FPLY\x03\x01\x03\0");
                    let mut out = b"FPLY\x03\x01\x04\0\0\0\0\x14".to_vec();
                    out.extend_from_slice(&body[144..]);
                    if mode == Mode::BadConfirmation {
                        out[31] ^= 1;
                    }
                    out
                }
            }
            ("SETUP", _) => {
                let d = decode(&body);
                if !d.contains_key("streams") {
                    let uuid = d["sessionUUID"].as_string().unwrap();
                    assert_eq!(uuid.len(), 36);
                    assert_eq!(uuid.as_bytes()[14], b'4');
                    assert_eq!(d["ekey"].as_data().unwrap().len(), 72);
                    assert_eq!(d["eiv"].as_data().unwrap().len(), 16);
                    if mode == Mode::LegacyOrder {
                        status = 405;
                        Vec::new()
                    } else if mode == Mode::MalformedSetup {
                        b"bad plist".to_vec()
                    } else if mode == Mode::EmptyControl {
                        Vec::new()
                    } else {
                        bytes(dict([(
                            "skipRecord",
                            Value::Boolean(mode == Mode::SkipRecord),
                        )]))
                    }
                } else {
                    let stream = d["streams"].as_array().unwrap()[0].as_dictionary().unwrap();
                    assert!(!stream.contains_key("shk"));
                    assert!(!stream.contains_key("shiv"));
                    let kind = stream["type"].as_unsigned_integer().unwrap();
                    if mode == Mode::StallVideo && kind == 110 {
                        continue;
                    }
                    if kind == 96 {
                        assert_eq!(stream["ct"].as_unsigned_integer(), Some(2));
                        assert_eq!(stream["sr"].as_unsigned_integer(), Some(44100));
                        assert_eq!(stream["spf"].as_unsigned_integer(), Some(352));
                    } else {
                        assert_eq!(kind, 110);
                        let timestamps = stream["timestampInfo"].as_array().unwrap();
                        let names = timestamps
                            .iter()
                            .map(|v| v.as_dictionary().unwrap()["name"].as_string().unwrap())
                            .collect::<Vec<_>>();
                        assert_eq!(names, ["SubSu", "BePxT", "AfPxT", "BefEn", "EmEnc"]);
                        if mode == Mode::LegacyOrder {
                            assert!(d.contains_key("ekey"));
                        }
                        if mode == Mode::RejectVideo {
                            status = 500;
                        }
                    }
                    if (mode == Mode::EmptyVideo && kind == 110)
                        || (mode == Mode::EmptyAudio && kind == 96)
                    {
                        Vec::new()
                    } else {
                        bytes(dict([(
                            "streams",
                            Value::Array(vec![dict([
                                ("type", int(kind)),
                                ("dataPort", int(if kind == 110 { 7001 } else { 7002 })),
                                ("controlPort", int(7003)),
                            ])]),
                        )]))
                    }
                }
            }
            ("RECORD", _) | ("TEARDOWN", _) => Vec::new(),
            _ => panic!("unexpected request {method} {path}"),
        };
        respond(&mut s, seq, status, &reply).await;
        if method == "TEARDOWN" {
            return;
        }
    }
}
