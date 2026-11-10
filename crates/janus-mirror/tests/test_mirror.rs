//! Whole pipeline against a scripted Cast receiver: test pattern → H.264 →
//! OFFER/ANSWER → encrypted RTP over UDP → receiver reassembles, decrypts,
//! ACKs and decodes the video; Stop sends STOP to the receiver.
use janus_cast::{
    connection,
    offer::WEBRTC_NAMESPACE,
    rtp,
    session::{Connected, Session, SessionConfig, testing},
    trust::DevicePolicy,
    wire::{self, Message, Payload},
};
use janus_mirror::{Options, Source};
use openh264::decoder::Decoder;
use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{io::DuplexStream, net::UdpSocket, time::timeout};

const TRANSPORT: &str = "web-9";

#[derive(Default)]
struct Shared {
    video: Option<(u32, [u8; 16], [u8; 16])>, // ssrc, key, mask
    stopped: bool,
}

fn reply(source: &str, namespace: &str, body: serde_json::Value) -> Message {
    Message::new(
        source,
        wire::SENDER,
        namespace,
        Payload::Text(body.to_string()),
    )
    .unwrap()
}

fn key(hex: &str) -> [u8; 16] {
    let bytes: Vec<u8> = (0..32)
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    bytes.try_into().unwrap()
}

async fn control(mut stream: DuplexStream, udp_port: u16, shared: Arc<Mutex<Shared>>) {
    while let Ok(message) = wire::read(&mut stream).await {
        let Payload::Text(body) = &message.payload else {
            continue;
        };
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        let request = json["requestId"].clone();
        let status = |running: bool| {
            let apps = if running {
                serde_json::json!([{"appId":"0F5096E8","sessionId":"m-1","transportId":TRANSPORT}])
            } else {
                serde_json::json!([])
            };
            reply(
                wire::RECEIVER,
                wire::RECEIVER_NAMESPACE,
                serde_json::json!({"type":"RECEIVER_STATUS","requestId":request,"status":{"applications":apps}}),
            )
        };
        let answer = match json["type"].as_str().unwrap_or_default() {
            "GET_STATUS" => Some(status(false)),
            "PING" => Some(reply(
                wire::RECEIVER,
                wire::HEARTBEAT_NAMESPACE,
                serde_json::json!({"type":"PONG"}),
            )),
            "GET_APP_AVAILABILITY" => Some(reply(
                wire::RECEIVER,
                wire::RECEIVER_NAMESPACE,
                serde_json::json!({"responseType":"GET_APP_AVAILABILITY","requestId":request,
                    "availability":{"0F5096E8":"APP_AVAILABLE","85CDB22F":"APP_AVAILABLE"}}),
            )),
            "LAUNCH" => Some(status(true)),
            "OFFER" => {
                let streams = json["offer"]["supportedStreams"]
                    .as_array()
                    .unwrap()
                    .clone();
                let video = streams
                    .iter()
                    .find(|s| s["type"] == "video_source")
                    .unwrap();
                assert_eq!(video["codecName"], "h264");
                shared.lock().unwrap().video = Some((
                    video["ssrc"].as_u64().unwrap() as u32,
                    key(video["aesKey"].as_str().unwrap()),
                    key(video["aesIvMask"].as_str().unwrap()),
                ));
                Some(reply(
                    TRANSPORT,
                    WEBRTC_NAMESPACE,
                    serde_json::json!({
                    "type":"ANSWER","seqNum":json["seqNum"],"result":"ok",
                    "answer":{"udpPort":udp_port,"sendIndexes":[video["index"]],"ssrcs":[77]}}),
                ))
            }
            "STOP" => {
                shared.lock().unwrap().stopped = true;
                None
            }
            _ => None,
        };
        if let Some(answer) = answer
            && wire::write(&mut stream, &answer).await.is_err()
        {
            return;
        }
    }
}

/// Cast feedback acknowledging everything up to `frame`.
fn ack(receiver: u32, sender: u32, frame: u8) -> Vec<u8> {
    let mut out = vec![0x8f, 206, 0, 4];
    out.extend(receiver.to_be_bytes());
    out.extend(sender.to_be_bytes());
    out.extend(b"CAST");
    out.extend([frame, 0, 0x01, 0x90]);
    out
}

#[tokio::test]
async fn test_test_pattern_casts_as_decodable_h264_and_stops_cleanly() {
    let media = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = media.local_addr().unwrap().port();
    let shared = Arc::new(Mutex::new(Shared::default()));
    let control_shared = shared.clone();
    let session = testing::start_with(
        &tokio::runtime::Handle::current(),
        SessionConfig::default(),
        move || {
            let shared = control_shared.clone();
            async move {
                let (client, server) = tokio::io::duplex(1 << 20);
                tokio::spawn(control(server, port, shared));
                Ok::<_, connection::Error>(Connected {
                    stream: client,
                    policy: DevicePolicy::Unrestricted,
                    peer: IpAddr::V4(Ipv4Addr::LOCALHOST),
                })
            }
        },
    );
    timeout(Duration::from_secs(5), async {
        let mut updates = session.subscribe();
        while !updates.borrow_and_update().ready_for_screen() {
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

    let options = Options {
        source: Source::TestPattern,
        audio: false,
        video_bit_rate: 2_000_000,
        ..Options::default()
    };
    let mirror = janus_mirror::start(&session, options)
        .await
        .expect("cast starts");
    let (ssrc, aes_key, mask) = shared.lock().unwrap().video.unwrap();
    let crypto = rtp::FrameCrypto::new(aes_key, mask);

    // Reassemble, decrypt, ACK and decode ~1 s of video.
    let mut decoder = Decoder::new().unwrap();
    let mut frames: BTreeMap<u8, BTreeMap<u16, Vec<u8>>> = BTreeMap::new();
    let mut decoded = 0;
    let mut first_key = None;
    let mut sender = None;
    let mut buffer = vec![0_u8; 2048];
    let mut expected_frame: i64 = 0;
    timeout(Duration::from_secs(10), async {
        while decoded < 20 {
            let (n, from) = media.recv_from(&mut buffer).await.unwrap();
            sender.get_or_insert(from);
            let packet = &buffer[..n];
            if packet[1] == 200
                || n < 19
                || u32::from_be_bytes(packet[8..12].try_into().unwrap()) != ssrc
            {
                continue;
            }
            let frame_id = packet[13];
            let packet_id = u16::from_be_bytes([packet[14], packet[15]]);
            let max_id = u16::from_be_bytes([packet[16], packet[17]]);
            let extensions = packet[12] & 0x3f;
            let payload = &packet[19 + 4 * usize::from(extensions)..];
            first_key.get_or_insert(packet[12] & 0x80 != 0);
            let parts = frames.entry(frame_id).or_default();
            parts.insert(packet_id, payload.to_vec());
            if parts.len() == usize::from(max_id) + 1
                && i64::from(frame_id) == expected_frame & 0xff
            {
                let mut data: Vec<u8> = frames
                    .remove(&frame_id)
                    .unwrap()
                    .into_values()
                    .flatten()
                    .collect();
                crypto.crypt(expected_frame, &mut data);
                if decoder.decode(&data).unwrap().is_some() {
                    decoded += 1;
                }
                media
                    .send_to(&ack(77, ssrc, frame_id), sender.unwrap())
                    .await
                    .unwrap();
                expected_frame += 1;
            }
        }
    })
    .await
    .expect("receiver decodes the cast");
    assert_eq!(first_key, Some(true), "stream starts with a key frame");

    let stats = mirror.stats().borrow().clone();
    assert!(stats.video_frames >= 20);
    assert!(stats.key_frames >= 1);
    assert_eq!((stats.width, stats.height), (1280, 720));

    mirror.stop(&session).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(shared.lock().unwrap().stopped, "Stop reaches the receiver");
    let _: Session = session;
}
