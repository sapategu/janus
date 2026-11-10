//! Narrow test facade: returns facts, never a live Sender or trust credential.
pub use crate::fairplay::{descriptor_vector, keywrap_vector, table_digest};
pub use crate::hisense::{canvas_size, idr_requested};
pub use crate::http::parse_capability as http_capability;
pub use crate::video::{framed_vector, hisense_vector, paired_vector};
pub use crate::wire::validate_plist;
use crate::{
    Error,
    session::{self, Setup},
    wire::Connection,
};
use tokio::io::DuplexStream;

pub fn http_timing_reply(request: &[u8], received: u64, sent: u64) -> Option<[u8; 48]> {
    crate::http::timing_reply(request, received, sent)
}
pub fn http_preamble() -> Vec<u8> {
    crate::http::stream_body(
        &session::StreamKey {
            raw: [0x42; 16],
            wrapped: [0x55; 72],
            iv: [0x66; 16],
        },
        0x021122334455,
        -42,
        90,
    )
    .expect("local descriptor")
}
pub fn http_frames(
    key: [u8; 16],
    iv: [u8; 16],
    frames: Vec<(Vec<u8>, bool)>,
) -> Result<Vec<Vec<u8>>, Error> {
    let mut video = crate::video::Video::http(key, iv, 1280, 720);
    let mut out = Vec::new();
    for (i, (frame, key)) in frames.into_iter().enumerate() {
        out.extend(video.packets(
            &frame,
            key,
            std::time::Duration::from_millis(500 + i as u64 * 33),
        )?);
    }
    Ok(out)
}
pub async fn http_start(authentication: DuplexStream, mirror: DuplexStream) -> Result<(), Error> {
    use tokio::io::AsyncWriteExt;
    let mut control = Connection::new(authentication);
    let info = session::get_info(&mut control).await?;
    let (mut stream, key, _) = crate::http::begin(
        &mut control,
        info.features,
        Connection::http(mirror, "10.1.1.1:7100".parse().expect("test endpoint")),
        0x021122334455,
        -42,
        90,
    )
    .await?;
    // Codec data opens the stream, then video, then heartbeats.
    let access_unit = [
        0, 0, 1, 0x67, 0x42, 0, 0x1f, 0x80, 0, 0, 1, 0x68, 0xce, 0x80, 0, 0, 1, 0x65, 0xaa,
    ];
    let mut video = crate::video::Video::http(key.raw, key.iv, 1280, 720);
    for packet in video.packets(&access_unit, true, std::time::Duration::ZERO)? {
        stream.write_all(&packet).await?;
    }
    let mut heartbeat = [0; 128];
    heartbeat[4] = 2;
    heartbeat[6] = 0x1e;
    stream.write_all(&heartbeat).await?;
    stream.shutdown().await?;
    Ok(())
}
/// Returns the monitor's error and its bounded, payload-free event labels.
pub async fn http_monitor<R: tokio::io::AsyncRead + Unpin>(
    stream: R,
    stop: tokio::sync::watch::Sender<bool>,
) -> (Option<Error>, Vec<String>) {
    let rx = stop.subscribe();
    let observed = crate::http::monitor(stream, stop, rx, std::time::Instant::now()).await;
    (observed.error, observed.events)
}

pub fn timing_reply(request: &[u8], timestamp: u64) -> Option<[u8; 32]> {
    crate::streaming::timing_reply(request, timestamp)
}

pub fn retransmission_vectors(
    packets: Vec<Vec<u8>>,
    requests: Vec<Vec<u8>>,
) -> Result<Vec<Vec<Vec<u8>>>, Error> {
    let mut history = crate::audio::History::new();
    for packet in packets {
        history.remember(packet)?;
    }
    Ok(requests.iter().map(|r| history.responses(r)).collect())
}

pub async fn run_control(
    stream: DuplexStream,
    stop: tokio::sync::watch::Sender<bool>,
    stats: tokio::sync::watch::Sender<crate::Stats>,
) -> (Option<Error>, bool) {
    let rx = stop.subscribe();
    crate::streaming::control_loop(
        Connection::new(stream),
        "rtsp://10.1.1.1/session".into(),
        stop,
        rx,
        stats,
    )
    .await
}
pub async fn write_packet(
    mut stream: DuplexStream,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Result<(), Error> {
    crate::streaming::write_packet(&mut stream, &[1; 4096], &mut stop).await
}

pub fn framed_sequence(
    key: [u8; 16],
    id: u64,
    frames: Vec<(Vec<u8>, bool)>,
) -> Result<Vec<Vec<u8>>, Error> {
    let mut video = crate::video::Video::new(key, id, 1280, 720);
    let mut out = Vec::new();
    for (i, (frame, key)) in frames.into_iter().enumerate() {
        out.extend(video.packets(
            &frame,
            key,
            std::time::Duration::from_millis(500 + i as u64 * 33),
        )?);
    }
    Ok(out)
}
pub fn audio_packet(key: [u8; 16], iv: [u8; 16], samples: &[i16]) -> Result<Vec<u8>, Error> {
    crate::audio::Audio::new(key, iv, 0x12345678, 1).packet(samples, 1234)
}

#[derive(Debug)]
pub struct NegotiationFacts {
    pub video_port: u16,
    pub audio_port: Option<u16>,
    pub closed: bool,
}
pub async fn negotiate(stream: DuplexStream, audio: bool) -> Result<NegotiationFacts, Error> {
    let mut c = Connection::new(stream);
    let info = session::get_info(&mut c).await?;
    let n = session::negotiate(
        &mut c,
        &info,
        "10.1.1.1:7000".parse().expect("constant address"),
        Setup {
            audio,
            timing_port: 60000,
            control_port: 60001,
            delay_ms: 400,
        },
    )
    .await?;
    if !n.skip_record {
        c.request("RECORD", &n.uri, "", &[], &[]).await?;
    }
    let closed = c.request("TEARDOWN", &n.uri, "", &[], &[]).await.is_ok();
    Ok(NegotiationFacts {
        video_port: n.video_port,
        audio_port: n.audio_port,
        closed,
    })
}
pub async fn sequential_requests(stream: DuplexStream) -> Vec<Result<u16, Error>> {
    let mut c = Connection::new(stream);
    let mut out = Vec::new();
    for _ in 0..2 {
        out.push(
            c.request("GET", "/info", "", &[], &[])
                .await
                .map(|r| r.status),
        );
    }
    out
}
pub async fn probe(stream: DuplexStream) -> Result<crate::Capability, Error> {
    let mut c = Connection::new(stream);
    Ok(session::get_info(&mut c).await?.capability)
}

/// Packet types the real engine writes first when video starts after
/// `delay_before_frame`: codec data must precede heartbeats.
pub async fn http_engine_packet_types(
    delay_before_frame: std::time::Duration,
    access_unit: Vec<u8>,
    count: usize,
) -> Result<Vec<u8>, Error> {
    crate::streaming::engine_packet_types(delay_before_frame, access_unit, count).await
}

// ---- HomeKit-style pairing (facts only; no live sender or stored identity) ----

pub fn tlv(items: &[(u8, &[u8])]) -> Vec<u8> {
    crate::hap::tlv(items)
}
pub fn parse_tlv(bytes: &[u8]) -> Result<std::collections::BTreeMap<u8, Vec<u8>>, Error> {
    crate::hap::parse_tlv(bytes)
}
/// SRP-6a client outputs: (A, M1, K, expected M2).
pub type SrpFacts = (Vec<u8>, [u8; 64], [u8; 64], [u8; 64]);
/// (identity id, [(deviceID, receiver id, receiver key)]).
pub type StoreFacts = (String, Vec<(String, Vec<u8>, [u8; 32])>);
pub fn srp_vector(
    pin: &[u8],
    salt: &[u8],
    server: &[u8],
    private: &[u8; 32],
) -> Result<SrpFacts, Error> {
    let s = crate::hap::srp(pin, salt, server, private)?;
    Ok((s.public, s.proof, s.session, s.server_proof))
}
/// (control read, control write, events read, events write, video key 1).
pub fn channel_keys(shared: &[u8]) -> [[u8; 32]; 5] {
    let control = crate::hap::control_keys(shared);
    let events = crate::hap::event_keys(shared);
    [
        control.read,
        control.write,
        events.read,
        events.write,
        crate::hap::video_key(shared, 1),
    ]
}
/// Facts from one PIN pair-setup + pair-verify with a fresh sender identity
/// against `stream`: (sender id, sender long-term public key, receiver id,
/// receiver long-term key, pair-verify shared secret).
pub async fn pin_pairing(
    stream: DuplexStream,
    screen_capture: bool,
    pin: &str,
) -> Result<(String, Vec<u8>, Vec<u8>, [u8; 32], [u8; 32]), Error> {
    use ring::signature::KeyPair;
    let kind = if screen_capture {
        crate::hap::Kind::ScreenCapture
    } else {
        crate::hap::Kind::Sdk
    };
    let identity = crate::hap::Identity::generate()?;
    let public = ring::signature::Ed25519KeyPair::from_seed_unchecked(&identity.seed)
        .map_err(|_| Error::Entropy)?
        .public_key()
        .as_ref()
        .to_vec();
    let mut c = Connection::new(stream);
    crate::hap::start_pin(&mut c, kind, &identity).await?;
    let peer = crate::hap::setup(&mut c, kind, &identity, pin).await?;
    let shared = crate::hap::verify(&mut c, kind, &identity, &peer).await?;
    Ok((identity.id.clone(), public, peer.id, peer.key, shared))
}
/// Pair-verify only, against a receiver identity the caller supplies.
pub async fn pair_verify(
    stream: DuplexStream,
    seed: [u8; 32],
    id: &str,
    peer_id: &[u8],
    peer_key: [u8; 32],
) -> Result<[u8; 32], Error> {
    let identity = crate::hap::Identity {
        id: id.to_owned(),
        seed,
    };
    let peer = crate::hap::Peer {
        id: peer_id.to_vec(),
        key: peer_key,
    };
    let mut c = Connection::new(stream);
    crate::hap::verify(&mut c, crate::hap::Kind::Sdk, &identity, &peer).await
}
/// Writes `message` through the HAP framing, then reads `reply_len`
/// decrypted bytes back from the same channel.
pub async fn secure_exchange(
    stream: DuplexStream,
    read: [u8; 32],
    write: [u8; 32],
    message: &[u8],
    reply_len: usize,
) -> Result<Vec<u8>, Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = crate::secure::Secure::new(stream);
    s.enable(crate::hap::ChannelKeys { read, write })?;
    s.write_all(message).await?;
    s.flush().await?;
    let mut reply = vec![0; reply_len];
    s.read_exact(&mut reply).await?;
    Ok(reply)
}
/// (CSeq, bytes consumed) of the first complete receiver event request.
pub fn event_request(buf: &[u8]) -> Result<Option<(Option<u32>, usize)>, Error> {
    crate::streaming::event_request(buf)
}
/// Saves then reloads a pairing store at `path`: (identity id, receivers).
pub fn store_roundtrip(
    path: &std::path::Path,
    device: &str,
    peer_id: &[u8],
    peer_key: [u8; 32],
) -> Result<StoreFacts, Error> {
    let mut store = crate::pairings::Store::load_or_new(path)?;
    store.receivers.insert(
        device.to_owned(),
        crate::hap::Peer {
            id: peer_id.to_vec(),
            key: peer_key,
        },
    );
    store.save(path)?;
    let loaded = crate::pairings::Store::load(path)?.ok_or(Error::Protocol)?;
    Ok((
        loaded.identity.id.clone(),
        loaded
            .receivers
            .iter()
            .map(|(d, p)| (d.clone(), p.id.clone(), p.key))
            .collect(),
    ))
}
/// Capability parsed from one /info plist (no stored-pairing lookup).
pub fn capability_from_info(body: &[u8]) -> Result<crate::Capability, Error> {
    let d = crate::wire::decode(body)?;
    Ok(session::Info::parse(&d)?.capability)
}
/// One paired-profile ALAC audio packet (RTP ‖ sealed ‖ tag ‖ LE64 nonce).
pub fn paired_audio_packet(key: [u8; 32], samples: &[i16], time: u32) -> Result<Vec<u8>, Error> {
    crate::audio::Audio::paired(key, 0x11223344, 7).packet(samples, time)
}

pub async fn hisense_negotiate(stream: DuplexStream) -> Result<(u16, Option<u16>, bool), Error> {
    let ip = "192.168.200.163".parse().expect("constant address");
    let mut rtsp = crate::hisense::RtspControl::new(stream, ip);
    let negotiated = crate::hisense::negotiate(&mut rtsp).await?;
    let closed = rtsp.teardown().await.is_ok();
    Ok((negotiated.video_port, negotiated.event_port, closed))
}

/// Explicit server identity required to select EShare instead of AirPlay.
pub fn hisense_server_identity(value: &str) -> bool {
    crate::hisense::eshare_server(value)
}

/// Exercises production EShare endpoint routing and negotiation with local sockets.
pub async fn hisense_connect(
    stream: tokio::net::TcpStream,
    advertised: std::net::SocketAddr,
) -> Result<u16, Error> {
    let mut control = crate::hisense::connect_rtsp(Connection::new(stream), advertised).await?;
    let negotiated = crate::hisense::negotiate(&mut control).await?;
    control.teardown().await?;
    Ok(negotiated.video_port)
}

/// One EShare AAC setup; returns only the receiver's selected UDP port.
pub async fn hisense_audio_setup(stream: DuplexStream) -> Result<u16, Error> {
    let mut control = crate::hisense::RtspControl::new(stream, "10.0.0.2".parse().unwrap());
    control.setup_audio().await
}

pub fn hisense_audio_packet(data: &[u8], sequence: u16, millis: u32) -> Result<Vec<u8>, Error> {
    crate::hisense::audio_packet(data, sequence, millis)
}
