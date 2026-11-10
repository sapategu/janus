//! Explicit cast flow against a scripted receiver: LAUNCH → mirroring app
//! status → CONNECT + OFFER to its transport → ANSWER → UDP media; Stop,
//! launch errors, rejected answers, timeouts and receiver-ended casts.
use janus_cast::{
    connection,
    offer::{Media, StreamOffer, WEBRTC_NAMESPACE},
    session::{CastError, CastPhase, Connected, Phase, Session, SessionConfig, Snapshot, testing},
    streaming::EncodedFrame,
    trust::DevicePolicy,
    wire::{self, Message, Payload},
};
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{io::DuplexStream, net::UdpSocket, time::timeout};

const TRANSPORT: &str = "web-7";
const CONFIG: SessionConfig = SessionConfig {
    ping_interval: Duration::from_millis(50),
    liveness_timeout: Duration::from_secs(2),
    max_reconnects: 0,
    reconnect_backoff: Duration::from_millis(10),
    cast_timeout: Duration::from_millis(400),
};

#[derive(Clone, Copy, PartialEq)]
enum Behaviour {
    Normal,
    LaunchError,
    RejectOffer,
    IgnoreLaunch,
    EndAfterAnswer,
}

type Log = Arc<Mutex<Vec<String>>>;

fn text(source: &str, namespace: &str, body: serde_json::Value) -> Message {
    Message::new(
        source,
        wire::SENDER,
        namespace,
        Payload::Text(body.to_string()),
    )
    .unwrap()
}

fn status(request: &serde_json::Value, running: bool) -> Message {
    let apps = if running {
        serde_json::json!([{"appId":"0F5096E8","sessionId":"mirror-1","transportId":TRANSPORT,
            "namespaces":[{"name":WEBRTC_NAMESPACE}]}])
    } else {
        serde_json::json!([])
    };
    text(
        wire::RECEIVER,
        wire::RECEIVER_NAMESPACE,
        serde_json::json!({"type":"RECEIVER_STATUS","requestId":request,"status":{"applications":apps}}),
    )
}

async fn receiver(mut stream: DuplexStream, behaviour: Behaviour, udp_port: u16, log: Log) {
    let mut launched = false;
    while let Ok(message) = wire::read(&mut stream).await {
        let Payload::Text(body) = &message.payload else {
            continue;
        };
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        let kind = json["type"].as_str().unwrap_or_default().to_owned();
        log.lock().unwrap().push(format!(
            "{}>{}:{kind}",
            message.destination,
            message.namespace.rsplit('.').next().unwrap()
        ));
        let request = json["requestId"].clone();
        let mut replies = Vec::new();
        match kind.as_str() {
            "GET_STATUS" => replies.push(status(&request, launched)),
            "PING" => replies.push(text(wire::RECEIVER, wire::HEARTBEAT_NAMESPACE, serde_json::json!({"type":"PONG"}))),
            "GET_APP_AVAILABILITY" => replies.push(text(
                wire::RECEIVER,
                wire::RECEIVER_NAMESPACE,
                serde_json::json!({"responseType":"GET_APP_AVAILABILITY","requestId":request,
                    "availability":{"0F5096E8":"APP_AVAILABLE","85CDB22F":"APP_AVAILABLE"}}),
            )),
            "LAUNCH" => match behaviour {
                Behaviour::LaunchError => replies.push(text(
                    wire::RECEIVER,
                    wire::RECEIVER_NAMESPACE,
                    serde_json::json!({"type":"LAUNCH_ERROR","requestId":request,"reason":"NOT_ALLOWED"}),
                )),
                Behaviour::IgnoreLaunch => {}
                _ => {
                    launched = true;
                    replies.push(status(&request, true));
                }
            },
            "OFFER" => {
                assert_eq!(message.destination, TRANSPORT);
                let offer = &json["offer"];
                assert_eq!(offer["castMode"], "mirroring");
                let streams = offer["supportedStreams"].as_array().unwrap();
                let answer = if behaviour == Behaviour::RejectOffer {
                    serde_json::json!({"type":"ANSWER","seqNum":json["seqNum"],"result":"error",
                        "error":{"code":3,"description":"unsupported codec"}})
                } else {
                    serde_json::json!({"type":"ANSWER","seqNum":json["seqNum"],"result":"ok",
                        "answer":{"udpPort":udp_port,
                            "sendIndexes":streams.iter().map(|s| s["index"].clone()).collect::<Vec<_>>(),
                            "ssrcs":streams.iter().map(|s| s["ssrc"].as_u64().unwrap() + 1).collect::<Vec<_>>()}})
                };
                replies.push(text(TRANSPORT, WEBRTC_NAMESPACE, answer));
                if behaviour == Behaviour::EndAfterAnswer {
                    launched = false;
                    replies.push(status(&serde_json::json!(0), false));
                }
            }
            "STOP" => {
                assert_eq!(json["sessionId"], "mirror-1");
                launched = false;
            }
            _ => {}
        }
        for reply in replies {
            if wire::write(&mut stream, &reply).await.is_err() {
                return;
            }
        }
    }
}

async fn start(behaviour: Behaviour, policy: DevicePolicy) -> (Session, Log, UdpSocket) {
    let media = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = media.local_addr().unwrap().port();
    let log = Log::default();
    let log2 = log.clone();
    let session = testing::start_with(&tokio::runtime::Handle::current(), CONFIG, move || {
        let log = log2.clone();
        async move {
            let (client, server) = tokio::io::duplex(65_536);
            tokio::spawn(receiver(server, behaviour, port, log));
            Ok::<_, connection::Error>(Connected {
                stream: client,
                policy,
                peer: IpAddr::V4(Ipv4Addr::LOCALHOST),
            })
        }
    });
    wait(&session, |s| {
        s.ready_for_screen() || s.audio_mirroring != janus_cast::session::Capability::Unknown
    })
    .await;
    (session, log, media)
}

async fn wait(session: &Session, what: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let mut updates = session.subscribe();
    timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = updates.borrow_and_update().clone();
            if what(&snapshot) {
                return snapshot;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out; last {:?}", session.snapshot()))
}

fn video() -> StreamOffer {
    StreamOffer {
        index: 0,
        media: Media::Video {
            codec: "h264",
            max_frame_rate: 30,
            max_bit_rate: 4_000_000,
            width: 1280,
            height: 720,
        },
        ssrc: 60_001,
        target_delay_ms: 400,
        aes_key: [3; 16],
        aes_iv_mask: [4; 16],
    }
}

fn sent(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

#[tokio::test]
async fn test_nothing_is_launched_until_the_user_casts() {
    let (session, log, _media) = start(Behaviour::Normal, DevicePolicy::Unrestricted).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        sent(&log)
            .iter()
            .all(|m| !m.contains("LAUNCH") && !m.contains("OFFER"))
    );
    assert_eq!(session.snapshot().cast, CastPhase::Idle);
}

#[tokio::test]
async fn test_cast_launches_negotiates_streams_media_and_stops() {
    let (session, log, media) = start(Behaviour::Normal, DevicePolicy::Unrestricted).await;
    let streamer = session.cast(vec![video()]).await.expect("cast negotiated");
    assert_eq!(session.snapshot().cast, CastPhase::Streaming);
    let order = sent(&log);
    let launch = order
        .iter()
        .position(|m| m.ends_with("receiver:LAUNCH"))
        .unwrap();
    let connect = order
        .iter()
        .position(|m| m == "web-7>connection:CONNECT")
        .unwrap();
    let offer = order
        .iter()
        .position(|m| m == "web-7>webrtc:OFFER")
        .unwrap();
    assert!(launch < connect && connect < offer);

    let stream = streamer.stream(0).unwrap();
    streamer
        .send(
            stream,
            EncodedFrame {
                key_frame: true,
                rtp_timestamp: 1,
                data: vec![1; 3000],
            },
        )
        .unwrap();
    let mut buffer = [0_u8; 2048];
    let mut rtp_packets = 0;
    timeout(Duration::from_secs(2), async {
        while rtp_packets < 3 {
            let n = media.recv(&mut buffer).await.unwrap();
            if buffer[1] & 0x7f == 96 && n > 19 {
                rtp_packets += 1;
            }
        }
    })
    .await
    .expect("RTP reaches the answered UDP port");

    session.stop_cast();
    wait(&session, |s| s.cast == CastPhase::Stopped).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let order = sent(&log);
    assert!(order.iter().any(|m| m.ends_with("receiver:STOP")));
    assert!(order.iter().any(|m| m == "web-7>connection:CLOSE"));
}

#[tokio::test]
async fn test_launch_error_rejected_answer_and_timeout_fail_cleanly() {
    let (session, _, _media) = start(Behaviour::LaunchError, DevicePolicy::Unrestricted).await;
    assert_eq!(
        session.cast(vec![video()]).await.err(),
        Some(CastError::LaunchFailed("NOT_ALLOWED".into()))
    );

    let (session, log, _media) = start(Behaviour::RejectOffer, DevicePolicy::Unrestricted).await;
    let error = session.cast(vec![video()]).await.err().unwrap();
    assert!(
        matches!(&error, CastError::Rejected(detail) if detail.contains("unsupported codec")),
        "{error:?}"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        sent(&log).iter().any(|m| m.ends_with("receiver:STOP")),
        "rejected cast undoes the launch"
    );

    let (session, _, _media) = start(Behaviour::IgnoreLaunch, DevicePolicy::Unrestricted).await;
    assert_eq!(
        session.cast(vec![video()]).await.err(),
        Some(CastError::Timeout)
    );
    // The connection itself survives a failed cast.
    assert_eq!(session.snapshot().phase, Phase::Connected);
}

#[tokio::test]
async fn test_receiver_ending_mirroring_is_reported() {
    let (session, _, _media) = start(Behaviour::EndAfterAnswer, DevicePolicy::Unrestricted).await;
    let _streamer = session.cast(vec![video()]).await.unwrap();
    wait(&session, |s| s.cast == CastPhase::EndedByReceiver).await;
}

#[tokio::test]
async fn test_audio_only_device_cannot_cast_a_screen_and_one_cast_at_a_time() {
    let (session, _, _media) = start(Behaviour::Normal, DevicePolicy::AudioOnly).await;
    assert_eq!(
        session.cast(vec![video()]).await.err(),
        Some(CastError::ScreenUnsupported)
    );
    assert_eq!(session.cast(vec![]).await.err(), Some(CastError::NoStreams));

    let (session, _, _media) = start(Behaviour::Normal, DevicePolicy::Unrestricted).await;
    let _first = session.cast(vec![video()]).await.unwrap();
    assert_eq!(
        session.cast(vec![video()]).await.err(),
        Some(CastError::AlreadyCasting)
    );
}
