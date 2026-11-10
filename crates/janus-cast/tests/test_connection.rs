use bytes::Bytes;
use janus_cast::{connection::*, trust, wire};
use prost::Message as _;
use rustls::pki_types::PrivatePkcs8KeyDer;
use rustls::pki_types::{ServerName, UnixTime};
use std::{sync::Arc, time::Duration};
use tokio::time::timeout;

fn identity(start: i64, end: i64) -> rcgen::CertifiedKey<rcgen::KeyPair> {
    let mut params = rcgen::CertificateParams::new(vec!["receiver.local".into()]).unwrap();
    // Test dates use fixed Unix seconds converted by rcgen's time dependency
    // through its public date helper (reference midnight is 2026-11-09).
    params.not_before =
        rcgen::date_time_ymd(2026, 11, 9) + std::time::Duration::from_secs(start.unsigned_abs());
    if start < 0 {
        params.not_before = rcgen::date_time_ymd(2026, 11, 9)
            - std::time::Duration::from_secs(start.unsigned_abs());
    }
    params.not_after =
        rcgen::date_time_ymd(2026, 11, 9) + std::time::Duration::from_secs(end.unsigned_abs());
    if end < 0 {
        params.not_after =
            rcgen::date_time_ymd(2026, 11, 9) - std::time::Duration::from_secs(end.unsigned_abs());
    }
    let signing_key = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&signing_key).unwrap();
    rcgen::CertifiedKey { cert, signing_key }
}

#[test]
fn rejects_invalid_temporary_certificate_dates_bounds_and_trailing_bytes() {
    let now = UnixTime::since_unix_epoch(Duration::from_secs(1_794_182_400));
    for (start, end, valid) in [
        (-60, 3600, true),
        (60, 3600, false),
        (-3600, 0, false),
        (-60, 4 * 86400 + 1, false),
    ] {
        let identity = identity(start, end);
        assert_eq!(validate_tls_leaf(identity.cert.der(), now).is_ok(), valid);
    }
    let mut cert = identity(-60, 3600).cert.der().to_vec();
    cert.push(0);
    assert!(validate_tls_leaf(&cert, now).is_err());
    assert!(validate_tls_leaf(&vec![0; MAX_CERT_BYTES + 1], now).is_err());
}

#[derive(Clone, PartialEq, prost::Message)]
struct DeviceAuth {
    #[prost(message, optional, tag = "1")]
    challenge: Option<Request>,
    #[prost(message, optional, tag = "2")]
    response: Option<Response>,
}
#[derive(Clone, PartialEq, prost::Message)]
struct Request {
    #[prost(bytes = "vec", optional, tag = "2")]
    nonce: Option<Vec<u8>>,
}
#[derive(Clone, PartialEq, prost::Message)]
struct Response {
    #[prost(bytes = "vec", optional, tag = "1")]
    signature: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "2")]
    certificate: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "5")]
    nonce: Option<Vec<u8>>,
    #[prost(int32, optional, tag = "6")]
    hash: Option<i32>,
}

#[tokio::test]
async fn real_tls_auth_probe_returns_only_unverified_inputs_and_closes() {
    for (version, flood) in [
        (&rustls::version::TLS13, false),
        (&rustls::version::TLS12, false),
        (&rustls::version::TLS13, true),
    ] {
        timeout(Duration::from_secs(5), async {
            let mut params = rcgen::CertificateParams::new(vec!["receiver.local".into()]).unwrap();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap();
            // Start at this day's UTC midnight and expire within four days.
            let midnight = rcgen::date_time_ymd(1970, 1, 1)
                + Duration::from_secs(now.as_secs() / 86400 * 86400);
            params.not_before = midnight;
            params.not_after = midnight + Duration::from_secs(2 * 86400);
            let key = rcgen::KeyPair::generate().unwrap();
            let cert = params.self_signed(&key).unwrap();
            let tls_der = cert.der().to_vec();
            let config = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_protocol_versions(&[version])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
            )
            .unwrap();
            let (client, server) = tokio::io::duplex(131_072);
            let peer = async {
                let mut stream = tokio_rustls::TlsAcceptor::from(Arc::new(config))
                    .accept(server)
                    .await
                    .unwrap();
                let request = wire::read(&mut stream).await.unwrap();
                assert_eq!(request.namespace, wire::AUTH_NAMESPACE);
                let wire::Payload::Binary(bytes) = request.payload else {
                    panic!("not binary")
                };
                let nonce = DeviceAuth::decode(bytes.as_ref())
                    .unwrap()
                    .challenge
                    .unwrap()
                    .nonce
                    .unwrap();
                if flood {
                    let unrelated = wire::Message::new(
                        wire::RECEIVER,
                        wire::SENDER,
                        wire::HEARTBEAT_NAMESPACE,
                        wire::Payload::Text("{\"type\":\"PING\"}".into()),
                    )
                    .unwrap();
                    for _ in 0..8 {
                        wire::write(&mut stream, &unrelated).await.unwrap();
                    }
                    assert!(wire::read(&mut stream).await.is_err());
                    return nonce;
                }
                let response = DeviceAuth {
                    challenge: None,
                    response: Some(Response {
                        signature: Some(vec![1; 256]),
                        certificate: Some(b"untrusted-test-device".to_vec()),
                        nonce: Some(nonce.clone()),
                        hash: Some(1),
                    }),
                };
                let response = wire::Message::new(
                    wire::RECEIVER,
                    wire::SENDER,
                    wire::AUTH_NAMESPACE,
                    wire::Payload::Binary(Bytes::from(response.encode_to_vec())),
                )
                .unwrap();
                wire::write(&mut stream, &response).await.unwrap();
                // Only authentication was sent. No CONNECT, GET_STATUS or media
                // follows, and dropping the probe closes the stream.
                assert!(wire::read(&mut stream).await.is_err());
                nonce
            };
            let (result, nonce) = tokio::join!(
                probe_stream(client, ServerName::try_from("receiver.local").unwrap()),
                peer
            );
            if flood {
                assert!(matches!(result, Err(Error::MessageLimit)));
            } else {
                let credentials = result.unwrap();
                assert_eq!(&credentials.signed_input()[..16], nonce);
                assert_eq!(&credentials.signed_input()[16..], tls_der);
                assert_eq!(credentials.certificate(), b"untrusted-test-device");
                // Structurally valid but forged identity never verifies.
                assert_eq!(
                    trust::verify(&credentials, std::time::SystemTime::now()).err(),
                    Some(trust::Error::Parse)
                );
            }
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn bounds_and_stalled_handshake_fail_without_a_socket_or_trusted_state() {
    assert!(matches!(
        probe_authentication(&[]).await,
        Err(Error::Address)
    ));
    assert!(matches!(
        probe_authentication(&["0.0.0.0:8009".parse().unwrap()]).await,
        Err(Error::Address)
    ));
    let (client, _stalled) = tokio::io::duplex(1024);
    let result = probe_stream(client, ServerName::try_from("receiver.local").unwrap()).await;
    assert!(matches!(result, Err(Error::Timeout)));
}

#[tokio::test]
async fn cancellation_drops_the_pending_channel() {
    use tokio::io::AsyncReadExt;
    let (client, mut peer) = tokio::io::duplex(1024);
    let task = tokio::spawn(probe_stream(
        client,
        ServerName::try_from("receiver.local").unwrap(),
    ));
    tokio::task::yield_now().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let mut received = Vec::new();
    timeout(Duration::from_secs(1), peer.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert!(received.len() <= 1024);
}
