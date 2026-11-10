use janus_airplay::{
    Profile,
    testing::{canvas_size, hisense_negotiate, hisense_vector, idr_requested, validate_plist},
};
use plist::{Dictionary, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn hisense_profile_label() {
    assert_eq!(Profile::Hisense.label(), "Hisense interactive display");
}

/// Body recorded from the physical board's keep-alive OPTIONS reply
/// (10 November 2026); `idr_req` asks the sender for an immediate IDR.
#[test]
fn eshare_options_idr_request_is_parsed_from_board_body() {
    let board = br#"{"Framerate":"15","idr_req":"0","casting_win_height":"2160","feature":"1","Castnum":"1","casting_win_width":"3840","exclusive_screen":"0","bitrate":"0","i-interval":"0"}"#;
    assert!(!idr_requested(board));
    let asked = String::from_utf8_lossy(board).replace(r#""idr_req":"0""#, r#""idr_req":"1""#);
    assert!(idr_requested(asked.as_bytes()));
    let cases: [(&[u8], bool); 7] = [
        (br#"{"idr_req":1}"#, true),
        (br#"{"idr_req" : "1" }"#, true),
        (br#"{"idr_req":""}"#, false),
        (br#"{"idr_req":"x"}"#, false),
        (b"", false),
        (b"\xff\xfe", false),
        (br#"{"bitrate":"0"}"#, false),
    ];
    for (body, expected) in cases {
        assert_eq!(
            idr_requested(body),
            expected,
            "{:?}",
            String::from_utf8_lossy(body)
        );
    }
    // The board's own canvas from the same reply; invalid sizes are refused.
    assert_eq!(canvas_size(board), Some((3840, 2160)));
    assert_eq!(
        canvas_size(br#"{"casting_win_width":1921,"casting_win_height":"1081"}"#),
        Some((1920, 1080))
    );
    assert_eq!(
        canvas_size(br#"{"casting_win_width":"0","casting_win_height":"2160"}"#),
        None
    );
    assert_eq!(
        canvas_size(br#"{"casting_win_width":"99999","casting_win_height":"2160"}"#),
        None
    );
    assert_eq!(canvas_size(br#"{"casting_win_width":"3840"}"#), None);
    // Oversized bodies are ignored, never scanned.
    let mut big = br#"{"idr_req":"1","pad":""#.to_vec();
    big.resize(5000, b'a');
    assert!(!idr_requested(&big));
}

#[test]
fn hisense_video_packets_use_eshare_128_byte_header_and_annexb() {
    let access_unit = [
        0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1f, 0xe9, 0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80, 0, 0, 0, 1,
        0x65, 0x88, 0x84, 0x21,
    ];
    let packets = hisense_vector(&access_unit).expect("valid Annex-B access unit");
    assert_eq!(packets.len(), 2);

    let config_pkt = &packets[0];
    assert!(config_pkt.len() > 128);
    let config_len = u32::from_le_bytes(config_pkt[0..4].try_into().unwrap()) as usize;
    let config_type = u16::from_le_bytes(config_pkt[4..6].try_into().unwrap());
    let config_pts = u64::from_le_bytes(config_pkt[8..16].try_into().unwrap());
    assert_eq!(config_type, 0x0100);
    assert_eq!(config_pts, 500_000);
    assert_eq!(config_pkt.len(), 128 + config_len);
    assert_eq!(
        &config_pkt[128..],
        &[
            0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1f, 0xe9, 0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80
        ]
    );

    let frame_pkt = &packets[1];
    let frame_len = u32::from_le_bytes(frame_pkt[0..4].try_into().unwrap()) as usize;
    let frame_type = u16::from_le_bytes(frame_pkt[4..6].try_into().unwrap());
    let frame_pts = u64::from_le_bytes(frame_pkt[8..16].try_into().unwrap());
    assert_eq!(frame_type, 0x0101);
    assert_eq!(frame_pts, 500_000);
    assert_eq!(frame_pkt.len(), 128 + frame_len);
    assert_eq!(&frame_pkt[128..], &[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21]);
}

async fn read_rtsp_request<S: tokio::io::AsyncRead + Unpin>(stream: &mut S) -> (String, Vec<u8>) {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        header.push(stream.read_u8().await.expect("header byte"));
    }
    let text = String::from_utf8(header).expect("utf8 header");
    let content_len = text
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let mut body = vec![0u8; content_len];
    if content_len > 0 {
        stream.read_exact(&mut body).await.expect("request body");
    }
    (text, body)
}

#[tokio::test]
async fn hisense_rtsp_negotiates_single_step_video_setup_and_teardown() {
    let (client, mut server) = tokio::io::duplex(16 * 1024);
    let responder = tokio::spawn(async move {
        // 1. OPTIONS
        let (opt_hdr, opt_body) = read_rtsp_request(&mut server).await;
        assert!(opt_hdr.starts_with("OPTIONS rtsp://192.168.200.163 RTSP/1.0\r\n"));
        assert!(opt_hdr.contains("User-Agent: EShare (EShare 1.0)\r\n"));
        assert!(opt_body.is_empty());
        server
            .write_all(b"RTSP/1.0 200 OK\r\nServer: AirTunes/211.3\r\nCSeq: 1\r\n\r\n")
            .await
            .unwrap();

        // 2. SETUP
        let (setup_hdr, setup_body) = read_rtsp_request(&mut server).await;
        assert!(setup_hdr.starts_with("SETUP rtsp://192.168.200.163 RTSP/1.0\r\n"));
        let device_name = format!("DeviceName: {}\r\n", janus_media::identity::sender_name());
        assert!(setup_hdr.contains(&device_name), "{setup_hdr}");
        assert!(setup_hdr.contains("SwitchFullscreen: 1\r\n"));
        assert!(validate_plist(&setup_body));
        let setup_plist =
            Value::from_reader(std::io::Cursor::new(&setup_body)).expect("valid setup plist");
        let streams = setup_plist
            .as_dictionary()
            .and_then(|d| d.get("streams"))
            .and_then(Value::as_array)
            .expect("streams array");
        let first = streams[0].as_dictionary().expect("stream dict");
        assert_eq!(
            first.get("type").and_then(Value::as_unsigned_integer),
            Some(110)
        );
        assert_eq!(
            first.get("androdstream").and_then(Value::as_boolean),
            Some(true)
        );
        assert_eq!(
            first.get("MacOSXStream").and_then(Value::as_boolean),
            Some(true)
        );

        let mut stream_reply = Dictionary::new();
        stream_reply.insert("type".into(), Value::Integer(110.into()));
        stream_reply.insert("dataPort".into(), Value::Integer(51030.into()));
        let mut root_reply = Dictionary::new();
        root_reply.insert(
            "streams".into(),
            Value::Array(vec![Value::Dictionary(stream_reply)]),
        );
        root_reply.insert("eventPort".into(), Value::Integer(51041.into()));
        let mut reply_bytes = Vec::new();
        Value::Dictionary(root_reply)
            .to_writer_binary(&mut reply_bytes)
            .unwrap();
        let reply_hdr = format!(
            "RTSP/1.0 200 OK\r\nServer: AirTunes/211.3\r\nCSeq: 2\r\nSession: DEADBEEF\r\nContent-Type: application/x-apple-binary-plist\r\nContent-Length: {}\r\n\r\n",
            reply_bytes.len()
        );
        server.write_all(reply_hdr.as_bytes()).await.unwrap();
        server.write_all(&reply_bytes).await.unwrap();

        // 3. TEARDOWN
        let (td_hdr, td_body) = read_rtsp_request(&mut server).await;
        assert!(td_hdr.starts_with("TEARDOWN rtsp://192.168.200.163 RTSP/1.0\r\n"));
        assert!(td_hdr.contains("Session: DEADBEEF\r\n"));
        assert!(validate_plist(&td_body));
        server
            .write_all(b"RTSP/1.0 200 OK\r\nServer: AirTunes/211.3\r\nCSeq: 3\r\n\r\n")
            .await
            .unwrap();
    });

    let (video_port, event_port, closed) = hisense_negotiate(client)
        .await
        .expect("negotiation succeeds");
    assert_eq!(video_port, 51030);
    assert_eq!(event_port, Some(51041));
    assert!(closed);
    responder.await.unwrap();
}

#[test]
fn hisense_requires_explicit_eshare_server_identity() {
    use janus_airplay::testing::hisense_server_identity;
    for value in ["EShare Http Server/1.0", "EShare/1.0", " eshare "] {
        assert!(hisense_server_identity(value), "{value}");
    }
    for value in [
        "AirTunes/211.3",
        "211.3",
        "AirPlay/220.68",
        "unrelated eshare",
        "",
    ] {
        assert!(!hisense_server_identity(value), "{value}");
    }
}

async fn answer_hisense_session(mut stream: tokio::net::TcpStream) {
    for (seq, method) in [(1, "OPTIONS"), (2, "SETUP"), (3, "TEARDOWN")] {
        let (header, _) = read_rtsp_request(&mut stream).await;
        assert!(header.starts_with(&format!("{method} rtsp://127.0.0.1 RTSP/1.0\r\n")));
        assert!(header.contains(&format!("CSeq: {seq}\r\n")));
        stream
            .write_all(
                format!("RTSP/1.0 200 OK\r\nCSeq: {seq}\r\nContent-Length: 0\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn hisense_routes_to_mirroring_port_and_never_retries_rtsp_rejection() {
    use janus_airplay::{Error, testing::hisense_connect};
    use std::time::Duration;
    use tokio::net::{TcpListener, TcpStream};

    // The production protocol fixes 51040; only the advertised service uses an
    // ephemeral test port. One test owns the fixed listener throughout.
    let primary = TcpListener::bind("127.0.0.1:51040").await.unwrap();
    let advertised = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = advertised.local_addr().unwrap();
    let client = TcpStream::connect(endpoint).await.unwrap();
    let (mut advertised_peer, _) = advertised.accept().await.unwrap();
    let task = tokio::spawn(async move { hisense_connect(client, endpoint).await });
    let (stream, _) = primary.accept().await.unwrap();
    answer_hisense_session(stream).await;
    assert_eq!(task.await.unwrap().unwrap(), 51030);
    // The old /info connection was dropped without sending a video SETUP.
    assert_eq!(
        advertised_peer.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );

    // A protocol/authentication error is final, even though the advertised
    // AirPlay service remains available. Never retry SETUP on another service.
    let client = TcpStream::connect(endpoint).await.unwrap();
    let (mut advertised_peer, _) = advertised.accept().await.unwrap();
    let task = tokio::spawn(async move { hisense_connect(client, endpoint).await });
    let (mut stream, _) = primary.accept().await.unwrap();
    let (header, _) = read_rtsp_request(&mut stream).await;
    assert!(header.starts_with("OPTIONS "));
    stream
        .write_all(b"RTSP/1.0 403 Forbidden\r\nCSeq: 1\r\n\r\n")
        .await
        .unwrap();
    assert!(matches!(task.await.unwrap(), Err(Error::Status(403))));
    assert_eq!(
        advertised_peer.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );

    // An advertised mirroring endpoint is reused, rather than opened twice.
    let endpoint_primary = primary.local_addr().unwrap();
    let client = TcpStream::connect(endpoint_primary).await.unwrap();
    let (stream, _) = primary.accept().await.unwrap();
    let task = tokio::spawn(async move { hisense_connect(client, endpoint_primary).await });
    answer_hisense_session(stream).await;
    assert_eq!(task.await.unwrap().unwrap(), 51030);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), primary.accept())
            .await
            .is_err()
    );

    // A failed primary TCP connection may use the already-open advertised
    // connection (for receivers that actually expose EShare on that port).
    drop(primary);
    let client = TcpStream::connect(endpoint).await.unwrap();
    let (stream, _) = advertised.accept().await.unwrap();
    let task = tokio::spawn(async move { hisense_connect(client, endpoint).await });
    answer_hisense_session(stream).await;
    assert_eq!(task.await.unwrap().unwrap(), 51030);
}

#[test]
fn hisense_aac_header_matches_native_bytes_and_bounds() {
    use janus_airplay::testing::hisense_audio_packet;
    let payload = [0x21, 0x10, 0x56, 0xe5];
    let packet = hisense_audio_packet(&payload, 0x1234, 0x56789abc).unwrap();
    assert_eq!(
        &packet[..12],
        &[0x80, 0x60, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 1, 0, 0, 0]
    );
    assert_eq!(&packet[12..], &payload); // Raw AAC; no ADTS or AU length field.
    assert!(hisense_audio_packet(&[], 0, 0).is_err());
    assert!(hisense_audio_packet(&vec![0; 1461], 0, 0).is_err());
    assert_eq!(
        hisense_audio_packet(&vec![0; 1460], u16::MAX, u32::MAX)
            .unwrap()
            .len(),
        1472
    );
}

#[tokio::test]
async fn hisense_audio_negotiates_receiver_selected_port_and_rejects_bad_ports() {
    use janus_airplay::testing::hisense_audio_setup;
    // Port 54022 was selected by the real board; never substitute fixed 51020.
    for (kind, port, valid) in [
        (96, 54022, true),
        (96, 0, false),
        (96, 65536, false),
        (110, 51030, false),
    ] {
        let (client, mut server) = tokio::io::duplex(4096);
        let receiver = tokio::spawn(async move {
            let (header, body) = read_rtsp_request(&mut server).await;
            assert!(header.starts_with("SETUP rtsp://10.0.0.2 RTSP/1.0\r\n"));
            let body = Value::from_reader(std::io::Cursor::new(body)).unwrap();
            let stream = body.as_dictionary().unwrap()["streams"].as_array().unwrap()[0]
                .as_dictionary()
                .unwrap();
            assert_eq!(stream.len(), 2);
            assert_eq!(stream["type"].as_unsigned_integer(), Some(96));
            assert_eq!(
                stream["audioFormat"].as_unsigned_integer(),
                Some(0x0100_0000)
            );
            let mut stream = Dictionary::new();
            stream.insert("type".into(), Value::Integer(kind.into()));
            stream.insert("dataPort".into(), Value::Integer(port.into()));
            let mut root = Dictionary::new();
            root.insert(
                "streams".into(),
                Value::Array(vec![Value::Dictionary(stream)]),
            );
            let mut body = Vec::new();
            Value::Dictionary(root).to_writer_binary(&mut body).unwrap();
            server
                .write_all(
                    format!(
                        "RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            server.write_all(&body).await.unwrap();
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            hisense_audio_setup(client),
        )
        .await
        .unwrap();
        if valid {
            assert_eq!(result.unwrap(), 54022);
        } else {
            assert!(result.is_err());
        }
        receiver.await.unwrap();
    }
}
