//! Receiver session lifecycle against a scripted in-memory receiver. The
//! connector stands in for TCP/TLS/trust; real receivers are covered by the
//! opt-in LAN sweep (`janus-cli/tests/test_native_receivers.rs`).
use janus_cast::{
    connection,
    session::{
        Capability, Connected, Failure, Loss, Phase, Session, SessionConfig, Snapshot, testing,
    },
    trust::{self, DevicePolicy},
    wire::{self, Message, Payload},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{io::DuplexStream, time::timeout};

const LOCALHOST: std::net::IpAddr = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

#[derive(Clone, Copy)]
struct Script {
    app_active: bool,
    screen_app: &'static str,
    answer_pings: bool,
    close_after_connect: bool,
    garbage: bool,
    ping_sender_first: bool,
}

const ANSWERING: Script = Script {
    app_active: true,
    screen_app: "APP_AVAILABLE",
    answer_pings: true,
    close_after_connect: false,
    garbage: false,
    ping_sender_first: false,
};

const FAST: SessionConfig = SessionConfig {
    ping_interval: Duration::from_millis(40),
    liveness_timeout: Duration::from_millis(150),
    max_reconnects: 2,
    reconnect_backoff: Duration::from_millis(10),
    cast_timeout: Duration::from_millis(500),
};

type Log = Arc<Mutex<Vec<String>>>;

fn reply(namespace: &str, body: serde_json::Value) -> Message {
    Message::new(
        wire::RECEIVER,
        wire::SENDER,
        namespace,
        Payload::Text(body.to_string()),
    )
    .unwrap()
}

async fn receiver(mut stream: DuplexStream, script: Script, log: Log) {
    if script.ping_sender_first {
        let ping = reply(
            wire::HEARTBEAT_NAMESPACE,
            serde_json::json!({"type":"PING"}),
        );
        let _ = wire::write(&mut stream, &ping).await;
    }
    while let Ok(message) = wire::read(&mut stream).await {
        let Payload::Text(text) = &message.payload else {
            continue;
        };
        let body: serde_json::Value = serde_json::from_str(text).unwrap();
        let kind = body["type"].as_str().unwrap_or_default().to_owned();
        log.lock().unwrap().push(format!(
            "{}:{kind}",
            message.namespace.rsplit('.').next().unwrap()
        ));
        let request = body["requestId"].clone();
        let answer = match kind.as_str() {
            "CONNECT" if script.close_after_connect => Some(reply(
                wire::CONNECTION_NAMESPACE,
                serde_json::json!({"type":"CLOSE"}),
            )),
            "CONNECT" if script.garbage => {
                for _ in 0..40 {
                    let junk = reply(
                        wire::RECEIVER_NAMESPACE,
                        serde_json::json!({"type":"RECEIVER_STATUS"}),
                    );
                    let _ = wire::write(&mut stream, &junk).await;
                }
                None
            }
            "GET_STATUS" => {
                let apps = if script.app_active {
                    serde_json::json!([{"appId":"E8C28D3C","sessionId":"s","transportId":"t"}])
                } else {
                    serde_json::json!([])
                };
                Some(reply(
                    wire::RECEIVER_NAMESPACE,
                    serde_json::json!({"type":"RECEIVER_STATUS","requestId":request,
                        "status":{"applications":apps,"volume":{"level":0.4,"muted":false}}}),
                ))
            }
            "GET_APP_AVAILABILITY" => Some(reply(
                wire::RECEIVER_NAMESPACE,
                serde_json::json!({"responseType":"GET_APP_AVAILABILITY","requestId":request,
                    "availability":{"0F5096E8":script.screen_app,"85CDB22F":"APP_AVAILABLE"}}),
            )),
            "PING" if script.answer_pings => Some(reply(
                wire::HEARTBEAT_NAMESPACE,
                serde_json::json!({"type":"PONG"}),
            )),
            _ => None,
        };
        if let Some(answer) = answer
            && wire::write(&mut stream, &answer).await.is_err()
        {
            return;
        }
    }
}

/// Each connect attempt takes the next script (the last one repeats).
fn start(
    scripts: Vec<Script>,
    policy: DevicePolicy,
    config: SessionConfig,
) -> (Session, Log, Arc<AtomicUsize>) {
    let log = Log::default();
    let attempts = Arc::new(AtomicUsize::new(0));
    let (log2, attempts2) = (log.clone(), attempts.clone());
    let session = testing::start_with(&tokio::runtime::Handle::current(), config, move || {
        let n = attempts2.fetch_add(1, Ordering::SeqCst);
        let script = scripts[n.min(scripts.len() - 1)];
        let log = log2.clone();
        async move {
            let (client, server) = tokio::io::duplex(65_536);
            tokio::spawn(receiver(server, script, log));
            Ok::<_, connection::Error>(Connected {
                stream: client,
                policy,
                peer: LOCALHOST,
            })
        }
    });
    (session, log, attempts)
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

#[tokio::test]
async fn test_connects_reads_status_and_capability_and_stays_read_only() {
    let (session, log, _) = start(vec![ANSWERING], DevicePolicy::Unrestricted, FAST);
    let ready = wait(&session, |s| s.ready_for_screen() && s.heartbeats >= 3).await;
    assert_eq!(ready.generation, 1);
    assert_eq!(ready.receiver_app_active, Some(true));
    assert_eq!(ready.volume, Some(0.4));
    assert_eq!(ready.audio_mirroring, Capability::Available);
    session.close().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let sent = log.lock().unwrap().clone();
    assert_eq!(
        &sent[..3],
        [
            "connection:CONNECT",
            "receiver:GET_STATUS",
            "receiver:GET_APP_AVAILABILITY"
        ]
    );
    let allowed = [
        "connection:CONNECT",
        "connection:CLOSE",
        "receiver:GET_STATUS",
        "receiver:GET_APP_AVAILABILITY",
        "heartbeat:PING",
        "heartbeat:PONG",
    ];
    assert!(
        sent.iter().all(|m| allowed.contains(&m.as_str())),
        "{sent:?}"
    );
    assert_eq!(sent.last().map(String::as_str), Some("connection:CLOSE"));
}

#[tokio::test]
async fn test_audio_only_device_is_never_ready_for_a_screen() {
    let (session, _, _) = start(vec![ANSWERING], DevicePolicy::AudioOnly, FAST);
    let snapshot = wait(&session, |s| s.audio_mirroring != Capability::Unknown).await;
    assert_eq!(snapshot.screen_mirroring, Capability::Unavailable);
    assert!(!snapshot.ready_for_screen());
}

#[tokio::test]
async fn test_receiver_without_mirroring_app_is_not_ready() {
    let script = Script {
        screen_app: "APP_UNAVAILABLE",
        ..ANSWERING
    };
    let (session, _, _) = start(vec![script], DevicePolicy::Unrestricted, FAST);
    let snapshot = wait(&session, |s| s.screen_mirroring != Capability::Unknown).await;
    assert_eq!(snapshot.screen_mirroring, Capability::Unavailable);
    assert!(!snapshot.ready_for_screen());
}

#[tokio::test]
async fn test_receiver_ping_is_answered() {
    let script = Script {
        ping_sender_first: true,
        ..ANSWERING
    };
    let (session, _, _) = start(vec![script], DevicePolicy::Unrestricted, FAST);
    wait(&session, |s| s.phase == Phase::Connected).await;
    // The scripted receiver logs nothing for PONG, but the session must keep
    // running (not treat the receiver's PING as a protocol error).
    let later = wait(&session, |s| s.heartbeats >= 2).await;
    assert_eq!(later.generation, 1);
}

#[tokio::test]
async fn test_liveness_loss_reconnects_with_new_generation_then_gives_up() {
    let silent = Script {
        answer_pings: false,
        ..ANSWERING
    };
    let (session, _, attempts) = start(vec![silent], DevicePolicy::Unrestricted, FAST);
    let reconnected = wait(&session, |s| s.generation >= 2).await;
    assert_eq!(reconnected.last_loss, Some(Loss::Timeout));
    let failed = wait(&session, |s| matches!(s.phase, Phase::Failed(_))).await;
    assert_eq!(failed.phase, Phase::Failed(Failure::Lost(Loss::Timeout)));
    // A receiver that never answers heartbeats cannot refill the retry budget:
    // exactly one initial connection plus `max_reconnects`.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1 + FAST.max_reconnects as usize
    );
}

#[tokio::test]
async fn test_receiver_close_is_a_loss_and_reconnects() {
    let closing = Script {
        close_after_connect: true,
        ..ANSWERING
    };
    let (session, _, _) = start(vec![closing, ANSWERING], DevicePolicy::Unrestricted, FAST);
    let snapshot = wait(&session, |s| {
        s.generation == 2 && s.phase == Phase::Connected
    })
    .await;
    assert_eq!(snapshot.last_loss, Some(Loss::ReceiverClosed));
}

#[tokio::test]
async fn test_malformed_message_flood_is_bounded() {
    let garbage = Script {
        garbage: true,
        ..ANSWERING
    };
    let (session, _, _) = start(vec![garbage, ANSWERING], DevicePolicy::Unrestricted, FAST);
    let snapshot = wait(&session, |s| s.generation == 2).await;
    assert_eq!(snapshot.last_loss, Some(Loss::Protocol));
}

#[tokio::test]
async fn test_trust_failure_is_final_and_never_retried() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    let session = testing::start_with(&tokio::runtime::Handle::current(), FAST, move || {
        counter.fetch_add(1, Ordering::SeqCst);
        async {
            Err::<Connected<DuplexStream>, _>(connection::Error::Untrusted(trust::Error::Revoked))
        }
    });
    let failed = wait(&session, |s| matches!(s.phase, Phase::Failed(_))).await;
    assert_eq!(
        failed.phase,
        Phase::Failed(Failure::Untrusted(trust::Error::Revoked))
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_unreachable_receiver_retries_boundedly() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    let session = testing::start_with(&tokio::runtime::Handle::current(), FAST, move || {
        counter.fetch_add(1, Ordering::SeqCst);
        async { Err::<Connected<DuplexStream>, _>(connection::Error::Connect) }
    });
    let failed = wait(&session, |s| matches!(s.phase, Phase::Failed(_))).await;
    assert_eq!(failed.phase, Phase::Failed(Failure::Unreachable));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1 + FAST.max_reconnects as usize
    );
}

#[tokio::test]
async fn test_no_endpoints_fails_without_spawning() {
    let session = Session::start(
        vec![],
        SessionConfig::default(),
        &tokio::runtime::Handle::current(),
    );
    assert_eq!(session.snapshot().phase, Phase::Failed(Failure::NoAddress));
}

#[tokio::test]
async fn test_drop_closes_the_channel() {
    let (session, log, _) = start(vec![ANSWERING], DevicePolicy::Unrestricted, FAST);
    wait(&session, |s| s.phase == Phase::Connected).await;
    drop(session);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let count = log.lock().unwrap().len();
    tokio::time::sleep(Duration::from_millis(200)).await;
    // No further pings after the handle is gone.
    assert_eq!(log.lock().unwrap().len(), count);
}
