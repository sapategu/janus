mod support;
use janus_airplay::{Error, testing};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[tokio::test]
async fn test_bad_framing_poisoned_connection_never_reused() {
    let cases = [
        "RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Length: 65537\r\n\r\n",
        "RTSP/1.0 200 OK\r\nCSeq: 999\r\nContent-Length: 0\r\n\r\n",
        "RTSP/1.0 200 OK\r\nContent-Length: 0\r\nContent-Length: 2\r\n\r\n",
        "RTSP/1.0 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n",
    ];
    assert_eq!(cases.len(), 4);
    for response in cases {
        let (client, mut server) = tokio::io::duplex(16384);
        let task = tokio::spawn(async move {
            support::request(&mut server).await.unwrap();
            server.write_all(response.as_bytes()).await.unwrap();
            assert_eq!(server.read(&mut [0; 8]).await.unwrap(), 0);
        });
        let results = testing::sequential_requests(client).await;
        assert_eq!(
            results,
            [
                Err(Error::InvalidResponse {
                    stage: "GET /info",
                    reason: "malformed RTSP headers or framing",
                }),
                Err(Error::Protocol),
            ]
        );
        task.await.unwrap();
    }
}
#[tokio::test(start_paused = true)]
async fn test_stalled_response_expires_and_cannot_be_reused() {
    let (client, _server) = tokio::io::duplex(16384);
    assert_eq!(
        testing::sequential_requests(client).await,
        [
            Err(Error::RequestTimeout { stage: "GET /info" }),
            Err(Error::Protocol)
        ]
    );
}
#[test]
fn test_binary_plist_bounds_depth_counts_and_input_format() {
    let valid = support::bytes(support::dict([("screen", plist::Value::Boolean(true))]));
    assert!(testing::validate_plist(&valid));
    assert!(!testing::validate_plist(b"<?xml version='1.0'?><plist/>"));
    assert!(!testing::validate_plist(&vec![0; 65537]));
    let mut nested = plist::Value::Boolean(true);
    for _ in 0..9 {
        nested = plist::Value::Array(vec![nested]);
    }
    assert!(!testing::validate_plist(&support::bytes(support::dict([
        ("deep", nested)
    ]))));
    let many = plist::Value::Array(vec![plist::Value::Boolean(true); 257]);
    assert!(!testing::validate_plist(&support::bytes(support::dict([
        ("many", many)
    ]))));
}
