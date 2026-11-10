mod support;
use janus_airplay::{Error, testing};
use std::sync::{Arc, Mutex};
use support::{Mode, receiver};
async fn run(mode: Mode, audio: bool) -> (Result<testing::NegotiationFacts, Error>, Vec<String>) {
    let (client, server) = tokio::io::duplex(128 * 1024);
    let log = Arc::new(Mutex::new(Vec::new()));
    let task = tokio::spawn(receiver(server, mode, log.clone()));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        testing::negotiate(client, audio),
    )
    .await
    .unwrap();
    task.await.unwrap();
    let log = log.lock().unwrap().clone();
    (result, log)
}
#[tokio::test]
async fn test_legacy_negotiation_authenticates_and_stops() {
    for audio in [false, true] {
        let (result, log) = run(Mode::Normal, audio).await;
        let facts = result.unwrap();
        assert_eq!(facts.video_port, 7001);
        assert_eq!(facts.audio_port, audio.then_some(7002));
        assert!(facts.closed);
        assert_eq!(log.last().unwrap(), "TEARDOWN session");
        assert_eq!(
            log.iter().filter(|s| s.starts_with("SETUP")).count(),
            if audio { 3 } else { 2 }
        );
        assert!(log.contains(&"RECORD session".into()));
    }
}
#[tokio::test]
async fn test_explicit_order_rejection_allows_one_legacy_setup() {
    let (result, log) = run(Mode::LegacyOrder, false).await;
    assert!(result.unwrap().closed);
    assert_eq!(log.iter().filter(|s| s.starts_with("SETUP")).count(), 2);
}
#[tokio::test]
async fn test_empty_successful_control_setup_continues_to_media() {
    for audio in [false, true] {
        let (result, log) = run(Mode::EmptyControl, audio).await;
        let facts = result.unwrap();
        assert_eq!(facts.video_port, 7001);
        assert_eq!(facts.audio_port, audio.then_some(7002));
        assert!(facts.closed);
        assert_eq!(
            log.iter().filter(|s| s.starts_with("SETUP")).count(),
            if audio { 3 } else { 2 }
        );
        assert!(log.contains(&"RECORD session".into()));
        assert_eq!(log.last().unwrap(), "TEARDOWN session");
    }
}
#[tokio::test]
async fn test_empty_media_setup_still_fails_and_tears_down() {
    for (mode, audio, stage) in [
        (Mode::EmptyVideo, false, "video SETUP"),
        (Mode::EmptyAudio, true, "audio SETUP"),
    ] {
        let (result, log) = run(mode, audio).await;
        assert_eq!(
            result.unwrap_err(),
            Error::InvalidResponse {
                stage,
                reason: "empty plist response body",
            }
        );
        assert!(!log.contains(&"RECORD session".into()));
        assert_eq!(log.last().unwrap(), "TEARDOWN session");
    }
}
#[tokio::test(start_paused = true)]
async fn test_video_setup_timeout_names_stage_and_closes_without_reuse() {
    let (client, server) = tokio::io::duplex(128 * 1024);
    let log = Arc::new(Mutex::new(Vec::new()));
    let task = tokio::spawn(receiver(server, Mode::StallVideo, log.clone()));
    let result = testing::negotiate(client, false).await;
    assert_eq!(
        result.unwrap_err(),
        Error::RequestTimeout {
            stage: "video SETUP"
        }
    );
    task.await.unwrap();
    let log = log.lock().unwrap();
    assert!(
        !log.iter()
            .any(|s| s.starts_with("RECORD") || s.starts_with("TEARDOWN"))
    );
    assert_eq!(log.iter().filter(|s| s.starts_with("SETUP")).count(), 2);
}
#[tokio::test]
async fn test_authentication_and_confirmation_fail_closed() {
    for mode in [Mode::BadProof, Mode::BadConfirmation] {
        let (result, log) = run(mode, false).await;
        assert!(matches!(result, Err(Error::Authentication)));
        assert!(!log.iter().any(|s| s.starts_with("SETUP")));
    }
}
#[tokio::test]
async fn test_failed_or_malformed_setup_is_torn_down() {
    for mode in [Mode::RejectVideo, Mode::MalformedSetup] {
        let (result, log) = run(mode, false).await;
        assert!(result.is_err());
        if mode == Mode::MalformedSetup {
            assert_eq!(
                result.unwrap_err(),
                Error::InvalidResponse {
                    stage: "control SETUP",
                    reason: "response body is not a binary plist",
                }
            );
        }
        assert_eq!(log.last().unwrap(), "TEARDOWN session");
        assert!(!log.contains(&"RECORD session".into()));
    }
}
#[tokio::test]
async fn test_pairing_and_audio_requirements_are_not_bypassed() {
    for (mode, error) in [
        (Mode::Pin, Error::PairingRequired),
        (Mode::NoAudio, Error::AudioUnsupported),
    ] {
        let (result, log) = run(mode, true).await;
        assert!(matches!(result,Err(e)if e==error));
        assert_eq!(log, ["GET /info"]);
    }
    let (result, _) = run(Mode::NoAudio, false).await;
    assert_eq!(result.unwrap().audio_port, None);
}
#[tokio::test]
async fn test_read_only_probe_never_starts_pairing_or_media() {
    let (client, server) = tokio::io::duplex(8192);
    let log = Arc::new(Mutex::new(Vec::new()));
    let task = tokio::spawn(receiver(server, Mode::Normal, log.clone()));
    let capability = testing::probe(client).await.unwrap();
    assert!(capability.screen && capability.audio);
    task.await.unwrap();
    assert_eq!(*log.lock().unwrap(), ["GET /info"]);
}

#[tokio::test]
async fn test_control_activation_waiver_survives_audio_setup() {
    for audio in [false, true] {
        let (result, log) = run(Mode::SkipRecord, audio).await;
        assert!(result.unwrap().closed);
        assert!(!log.iter().any(|s| s.starts_with("RECORD")));
    }
}

#[tokio::test]
async fn test_malformed_security_flags_fail_before_pairing() {
    use tokio::io::AsyncReadExt;
    let (client, mut server) = tokio::io::duplex(8192);
    let task = tokio::spawn(async move {
        let (method, path, seq, _) = support::request(&mut server).await.unwrap();
        assert_eq!((method.as_str(), path.as_str()), ("GET", "/info"));
        let info = support::bytes(support::dict([
            ("features", support::int(1 << 7)),
            ("statusFlags", plist::Value::String("512".into())),
        ]));
        support::respond(&mut server, seq, 200, &info).await;
        assert_eq!(server.read(&mut [0; 8]).await.unwrap(), 0);
    });
    assert!(matches!(
        testing::negotiate(client, false).await,
        Err(Error::InvalidResponse {
            stage: "GET /info",
            reason: "non-integer statusFlags",
        })
    ));
    task.await.unwrap();
}
