mod support;
use janus_airplay::{Error, testing};
use plist::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>width</key><integer>1920</integer><key>height</key><integer>1080</integer><key>refreshRate</key><real>0.016666666666666666</real><key>version</key><string>200.54</string></dict></plist>"#;

#[test]
fn test_http_capability_accepts_published_schema_and_bounds_untrusted_xml() {
    assert_eq!(
        testing::http_capability(XML.as_bytes()).unwrap(),
        (1920, 1080)
    );
    for bad in [
        XML.replace("1920", "999999"),
        XML.replace("0.016666666666666666", "NaN"),
        XML.replace("200.54", "unsupported/header\r\n"),
        XML.replace("<integer>1080</integer>", "<string>1080</string>"),
        "x".repeat(65537),
        "<?xml version=\"1.0\"?><plist><dict>".to_owned(),
    ] {
        assert!(testing::http_capability(bad.as_bytes()).is_err());
    }
    let entity = XML.replace("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">",
        "<!DOCTYPE plist [<!ENTITY bad SYSTEM 'file:///etc/passwd'>]>")
        .replace("200.54", "&bad;");
    assert!(testing::http_capability(entity.as_bytes()).is_err());
}

#[test]
fn test_http_preamble_wraps_key_and_never_exports_raw_key() {
    let bytes = testing::http_preamble();
    let d = Value::from_reader(std::io::Cursor::new(&bytes))
        .unwrap()
        .into_dictionary()
        .unwrap();
    assert_eq!(d["deviceID"].as_unsigned_integer(), Some(0x021122334455));
    assert_eq!(d["sessionID"].as_signed_integer(), Some(-42));
    assert_eq!(d["param1"].as_data().unwrap(), &[0x55; 72]);
    assert_eq!(d["param2"].as_data().unwrap(), &[0x66; 16]);
    assert_eq!(d["timestampInfo"].as_array().unwrap().len(), 7);
    assert_eq!(d["fpsInfo"].as_array().unwrap().len(), 8);
    assert!(!bytes.windows(16).any(|v| v == [0x42; 16]));
    assert!(!d.contains_key("shk"));
}

#[test]
fn test_http_video_uses_direct_continuous_ctr_and_legacy_header() {
    use aes::{
        Aes128,
        cipher::{KeyIvInit, StreamCipher},
    };
    use ctr::Ctr128BE;
    let frames = vec![
        (
            vec![
                0, 0, 1, 0x67, 0x42, 0, 0x1f, 0x80, 0, 0, 1, 0x68, 0xce, 0x80, 0, 0, 1, 0x65, 0xaa,
            ],
            true,
        ),
        (vec![0, 0, 1, 0x41, 0xbb, 0xcc], false),
    ];
    let packets = testing::http_frames([3; 16], [7; 16], frames).unwrap();
    assert_eq!(packets.len(), 3);
    assert_eq!(packets[0][4..8], [1, 0, 6, 0]);
    assert_eq!(packets[0][128..134], [1, 0x42, 0, 0x1f, 0xff, 0xe1]);
    let mut cipher = Ctr128BE::<Aes128>::new(&[3; 16].into(), &[7; 16].into());
    for (p, want) in [
        (&packets[1], vec![0, 0, 0, 2, 0x65, 0xaa]),
        (&packets[2], vec![0, 0, 0, 3, 0x41, 0xbb, 0xcc]),
    ] {
        assert_eq!(p[4..8], [0, 0, 6, 0]);
        assert_eq!(
            u32::from_le_bytes(p[..4].try_into().unwrap()) as usize,
            p.len() - 128
        );
        let mut data = p[128..].to_vec();
        assert_ne!(data, want);
        cipher.apply_keystream(&mut data);
        assert_eq!(data, want);
    }
}

#[test]
fn test_http_ntp_matches_48_byte_clock_and_ignores_responses() {
    let mut request = [0; 48];
    request[0] = 0x23;
    request[40..48].copy_from_slice(&0x0102030405060708u64.to_be_bytes());
    let reply =
        testing::http_timing_reply(&request, 0x1112131415161718, 0x2122232425262728).unwrap();
    assert_eq!(reply[0], 0x24);
    assert_eq!(reply[1], 1);
    assert_eq!(&reply[12..16], b"AIRP");
    assert_eq!(&reply[24..32], &request[40..48]);
    assert_eq!(&reply[32..40], &0x1112131415161718u64.to_be_bytes());
    assert_eq!(&reply[40..48], &0x2122232425262728u64.to_be_bytes());
    assert!(testing::http_timing_reply(&request[..47], 0, 0).is_none());
    assert!(testing::http_timing_reply(&reply, 0, 0).is_none());
    request[0] = 0x13;
    assert!(testing::http_timing_reply(&request, 0, 0).is_none());
}

async fn request(server: &mut DuplexStream) -> Option<(String, BTreeMap<String, String>, Vec<u8>)> {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        header.push(server.read_u8().await.ok()?);
        assert!(header.len() <= 8192);
    }
    let text = String::from_utf8(header).unwrap();
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap().to_owned();
    assert!(first.ends_with("HTTP/1.1"));
    let headers = lines
        .filter_map(|s| s.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
    let len = headers["content-length"].parse::<usize>().unwrap();
    assert!(len <= 65536);
    let mut body = vec![0; len];
    server.read_exact(&mut body).await.unwrap();
    Some((first, headers, body))
}
async fn http_peer(mut server: DuplexStream, bad_confirmation: bool) -> bool {
    let (line, h, body) = request(&mut server).await.unwrap();
    assert_eq!(line, "GET /stream.xml HTTP/1.1");
    assert!(body.is_empty());
    let response = format!(
        "HTTP/1.1 200 OK\r\nCSeq: {}\r\nContent-Type: text/x-apple-plist+xml\r\nContent-Length: {}\r\n\r\n{XML}",
        h["cseq"],
        XML.len()
    );
    server.write_all(response.as_bytes()).await.unwrap();
    let (_, h, body) = request(&mut server).await.unwrap();
    assert_eq!(body.len(), 16);
    let mut m2 = vec![0; 142];
    m2[..12].copy_from_slice(b"FPLY\x03\x01\x02\0\0\0\0\x82");
    m2[12] = 2;
    m2[13] = 3;
    let response = format!(
        "HTTP/1.1 200 OK\r\nCSeq: {}\r\nContent-Length: 142\r\n\r\n",
        h["cseq"]
    );
    server.write_all(response.as_bytes()).await.unwrap();
    server.write_all(&m2).await.unwrap();
    let (_, h, body) = request(&mut server).await.unwrap();
    assert_eq!(body.len(), 164);
    let mut m4 = b"FPLY\x03\x01\x04\0\0\0\0\x14".to_vec();
    m4.extend_from_slice(&body[144..164]);
    if bad_confirmation {
        m4[12] ^= 1;
    }
    let response = format!(
        "HTTP/1.1 200 OK\r\nCSeq: {}\r\nContent-Length: 32\r\n\r\n",
        h["cseq"]
    );
    server.write_all(response.as_bytes()).await.unwrap();
    server.write_all(&m4).await.unwrap();
    if bad_confirmation {
        assert_eq!(server.read(&mut [0; 8]).await.unwrap(), 0);
        return false;
    }
    let (line, h, body) = request(&mut server).await.unwrap();
    assert_eq!(line, "POST /stream HTTP/1.1");
    let d = Value::from_reader(std::io::Cursor::new(body))
        .unwrap()
        .into_dictionary()
        .unwrap();
    assert_eq!(
        d["deviceID"].as_unsigned_integer().unwrap(),
        u64::from_str_radix(h["x-apple-device-id"].trim_start_matches("0x"), 16).unwrap()
    );
    assert_eq!(d["param1"].as_data().unwrap().len(), 72);
    assert_eq!(d["param2"].as_data().unwrap().len(), 16);
    // No HTTP response is sent. Media must follow without waiting for one,
    // and codec data opens the stream (OpenAirPlay stream_packets).
    let mut types = Vec::new();
    for _ in 0..3 {
        let mut header = [0; 128];
        server.read_exact(&mut header).await.unwrap();
        let len = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        let mut payload = vec![0; len];
        server.read_exact(&mut payload).await.unwrap();
        types.push(header[4..8].to_vec());
    }
    assert_eq!(
        types,
        [vec![1, 0, 6, 0], vec![0, 0, 6, 0], vec![2, 0, 0x1e, 0]]
    );
    assert_eq!(server.read(&mut [0; 8]).await.unwrap(), 0);
    true
}
#[tokio::test]
async fn test_http_authenticates_before_upload_and_does_not_wait_for_start_response() {
    for bad in [false, true] {
        let (auth, auth_peer) = tokio::io::duplex(128 * 1024);
        let (mirror, peer) = tokio::io::duplex(128 * 1024);
        let log = Arc::new(Mutex::new(Vec::new()));
        let a = tokio::spawn(support::receiver(auth_peer, support::Mode::Normal, log));
        let b = tokio::spawn(http_peer(peer, bad));
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            testing::http_start(auth, mirror),
        )
        .await
        .unwrap();
        if bad {
            assert_eq!(result, Err(Error::Authentication));
        } else {
            assert!(result.is_ok());
        }
        a.await.unwrap();
        assert_eq!(b.await.unwrap(), !bad);
    }
}
#[tokio::test]
async fn test_http_never_contacts_mirroring_protocol_after_bad_raw_proof() {
    let (auth, auth_peer) = tokio::io::duplex(128 * 1024);
    let (mirror, mut peer) = tokio::io::duplex(128 * 1024);
    let a = tokio::spawn(support::receiver(
        auth_peer,
        support::Mode::BadProof,
        Arc::new(Mutex::new(Vec::new())),
    ));
    assert_eq!(
        testing::http_start(auth, mirror).await,
        Err(Error::Authentication)
    );
    assert_eq!(peer.read(&mut [0; 8]).await.unwrap(), 0);
    a.await.unwrap();
}
#[tokio::test]
async fn test_http_stream_permission_rejection_ends_sharing() {
    let (client, mut peer) = tokio::io::duplex(8192);
    let (stop, _) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(testing::http_monitor(client, stop.clone()));
    peer.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
        .await
        .unwrap();
    let (error, events) = task.await.unwrap();
    assert_eq!(error, Some(Error::PairingRequired));
    assert_eq!(events.len(), 1);
    assert!(events[0].starts_with("receiver replied HTTP 403 at "));
    assert!(*stop.borrow());
}
#[tokio::test]
async fn test_http_response_half_close_does_not_cancel_one_way_media_upload() {
    let (client, mut peer) = tokio::io::duplex(8192);
    let (read, mut write) = tokio::io::split(client);
    let (stop, _) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(testing::http_monitor(read, stop.clone()));
    peer.shutdown().await.unwrap();
    let (error, events) = task.await.unwrap();
    assert_eq!(error, None);
    assert_eq!(events.len(), 1);
    assert!(events[0].starts_with("receiver closed the stream connection (FIN) at "));
    assert!(!*stop.borrow());
    write.write_all(&[0x35; 128]).await.unwrap();
    let mut packet = [0; 128];
    peer.read_exact(&mut packet).await.unwrap();
    assert_eq!(packet, [0x35; 128]);
}
#[tokio::test(start_paused = true)]
async fn test_http_partial_reply_times_out_and_cancel_never_reuses_it() {
    for cancel in [false, true] {
        let (client, mut peer) = tokio::io::duplex(8192);
        let (stop, _) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(testing::http_monitor(client, stop.clone()));
        peer.write_all(b"HTTP/1.1 200 OK\r\nContent-Length:")
            .await
            .unwrap();
        if cancel {
            stop.send_replace(true);
        }
        let (result, _) = task.await.unwrap();
        if cancel {
            assert_eq!(result, None);
        } else {
            assert_eq!(
                result,
                Some(Error::RequestTimeout {
                    stage: "HTTP stream response"
                })
            );
        }
        assert_eq!(peer.read(&mut [0; 8]).await.unwrap(), 0);
    }
}
/// Before this fix a stopped engine discarded a reply that was already
/// readable, so a receiver's rejection surfaced only as a local Broken pipe.
#[tokio::test]
async fn test_http_ready_receiver_reply_is_recorded_even_after_local_stop() {
    let (client, mut peer) = tokio::io::duplex(8192);
    peer.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
        .await
        .unwrap();
    peer.shutdown().await.unwrap();
    let (stop, _) = tokio::sync::watch::channel(false);
    stop.send_replace(true);
    let (error, events) = testing::http_monitor(client, stop.clone()).await;
    assert_eq!(error, Some(Error::Status(400)));
    assert_eq!(events.len(), 1);
    assert!(events[0].starts_with("receiver replied HTTP 400 at "));
}
#[tokio::test]
async fn test_http_non_http_reply_is_counted_without_payload() {
    let (client, mut peer) = tokio::io::duplex(8192);
    peer.write_all(&[0xde, 0xad, 0xbe, 0xef]).await.unwrap();
    let (stop, _) = tokio::sync::watch::channel(false);
    let (error, events) = testing::http_monitor(client, stop.clone()).await;
    assert!(error.is_some());
    assert_eq!(events.len(), 1);
    assert!(events[0].starts_with("receiver sent 4 non-HTTP bytes at "));
    assert!(!events[0].contains("de"));
}
/// The real engine previously sent a heartbeat before codec data whenever
/// the first encoded frame arrived after the stream started.
#[tokio::test]
async fn test_http_engine_sends_codec_data_before_any_heartbeat() {
    let access_unit = vec![
        0, 0, 1, 0x67, 0x42, 0, 0x1f, 0x80, 0, 0, 1, 0x68, 0xce, 0x80, 0, 0, 1, 0x65, 0xaa,
    ];
    let types =
        testing::http_engine_packet_types(std::time::Duration::from_millis(1200), access_unit, 3)
            .await
            .unwrap();
    assert_eq!(types, [1, 0, 2]);
}
