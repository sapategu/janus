//! Cast Streaming sender: OFFER/ANSWER, encryption, packetization (checked
//! with the receiver-side rules of Open Screen `rtp_wire.rs`), RTCP and an
//! end-to-end loopback run against a scripted receiver.
use janus_cast::{
    offer::{self, Answer, AnswerError, Media, StreamOffer},
    rtcp, rtp,
    streaming::{EncodedFrame, StreamSpec, Streamer},
};
use std::time::Duration;
use tokio::{net::UdpSocket, time::timeout};

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn video(index: u32, ssrc: u32) -> StreamOffer {
    StreamOffer {
        index,
        media: Media::Video {
            codec: "h264",
            max_frame_rate: 30,
            max_bit_rate: 5_000_000,
            width: 1920,
            height: 1080,
        },
        ssrc,
        target_delay_ms: 400,
        aes_key: [7; 16],
        aes_iv_mask: [9; 16],
    }
}

fn audio(index: u32, ssrc: u32) -> StreamOffer {
    StreamOffer {
        index,
        media: Media::Audio {
            codec: "opus",
            bit_rate: 128_000,
            channels: 2,
        },
        ssrc,
        target_delay_ms: 400,
        aes_key: [1; 16],
        aes_iv_mask: [2; 16],
    }
}

#[test]
fn test_offer_uses_open_screen_field_names_and_android_tv_payload_types() {
    let message: serde_json::Value =
        serde_json::from_str(&offer::offer_message(5, &[audio(0, 11), video(1, 60_000)])).unwrap();
    assert_eq!(message["type"], "OFFER");
    assert_eq!(message["seqNum"], 5);
    assert_eq!(message["offer"]["castMode"], "mirroring");
    let streams = message["offer"]["supportedStreams"].as_array().unwrap();
    let (a, v) = (&streams[0], &streams[1]);
    assert_eq!(a["type"], "audio_source");
    assert_eq!(a["codecName"], "opus");
    assert_eq!(a["rtpPayloadType"], 127);
    assert_eq!(a["timeBase"], "1/48000");
    assert_eq!(a["channels"], 2);
    assert_eq!(a["bitRate"], 128_000);
    assert_eq!(v["type"], "video_source");
    assert_eq!(v["codecName"], "h264");
    assert_eq!(v["rtpPayloadType"], 96);
    assert_eq!(v["timeBase"], "1/90000");
    assert_eq!(v["maxFrameRate"], "30");
    assert_eq!(v["resolutions"][0]["width"], 1920);
    assert_eq!(v["aesKey"], "07".repeat(16));
    assert_eq!(v["aesIvMask"], "09".repeat(16));
    assert_eq!(v["rtpProfile"], "cast");
    assert_eq!(v["targetDelay"], 400);
    assert_eq!(v["receiverRtcpEventLog"], true);
    assert!(!format!("{:?}", video(1, 2)).contains("07, 07"));
}

#[test]
fn test_answer_must_match_sequence_and_offered_streams() {
    let offered = [audio(0, 11), video(1, 60_000)];
    let ok = r#"{"type":"ANSWER","seqNum":5,"result":"ok","answer":{"udpPort":2344,"sendIndexes":[1],"ssrcs":[60001]}}"#;
    assert_eq!(
        offer::parse_answer(ok, 5, &offered),
        Ok(Answer {
            udp_port: 2344,
            send_indexes: vec![1],
            ssrcs: vec![60001]
        })
    );
    assert_eq!(
        offer::parse_answer(ok, 6, &offered),
        Err(AnswerError::NotAnswer)
    );
    let rejected = r#"{"type":"ANSWER","seqNum":5,"result":"error","error":{"code":2,"description":"no codec"}}"#;
    assert!(
        matches!(offer::parse_answer(rejected, 5, &offered), Err(AnswerError::Rejected(d)) if d.contains("no codec"))
    );
    for bad in [
        r#"{"type":"ANSWER","seqNum":5,"result":"ok","answer":{"udpPort":0,"sendIndexes":[1],"ssrcs":[1]}}"#,
        r#"{"type":"ANSWER","seqNum":5,"result":"ok","answer":{"udpPort":9,"sendIndexes":[7],"ssrcs":[1]}}"#,
        r#"{"type":"ANSWER","seqNum":5,"result":"ok","answer":{"udpPort":9,"sendIndexes":[0,1],"ssrcs":[1]}}"#,
        r#"{"type":"ANSWER","seqNum":5,"result":"ok","answer":{"udpPort":9,"sendIndexes":[1],"ssrcs":[60000]}}"#,
        r#"{"type":"ANSWER","seqNum":5,"result":"ok"}"#,
        "not json",
    ] {
        assert!(offer::parse_answer(bad, 5, &offered).is_err(), "{bad}");
    }
}

#[test]
fn test_frame_crypto_matches_nist_aes128_ctr_with_frame_id_in_the_nonce() {
    // NIST SP 800-38A F.5.1: key, initial counter block, two blocks.
    let key: [u8; 16] = hex("2b7e151628aed2a6abf7158809cf4f3c").try_into().unwrap();
    let counter: [u8; 16] = hex("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff").try_into().unwrap();
    let plain = hex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51");
    let cipher = hex("874d6191b620e3261bef6864990db6ce9806f66b7970fdff8617187bb9fffdff");
    let frame_id: i64 = 0x0102_0304;
    // Choose the mask so mask XOR (frame id at bytes 8..12) is NIST's counter.
    let mut mask = counter;
    for (byte, id) in mask[8..12].iter_mut().zip((frame_id as u32).to_be_bytes()) {
        *byte ^= id;
    }
    let crypto = rtp::FrameCrypto::new(key, mask);
    let mut data = plain.clone();
    crypto.crypt(frame_id, &mut data);
    assert_eq!(data, cipher);
    crypto.crypt(frame_id, &mut data);
    assert_eq!(data, plain);
    let mut other = plain.clone();
    crypto.crypt(frame_id + 1, &mut other);
    assert_ne!(other, cipher);
}

/// Receiver-side parse (Open Screen rtp_wire.rs parse_rtp_packet_internal).
#[derive(Debug, PartialEq)]
struct Parsed {
    payload_type: u8,
    marker: bool,
    sequence: u16,
    rtp_timestamp: u32,
    key_frame: bool,
    frame_id: u8,
    packet_id: u16,
    max_packet_id: u16,
    referenced_frame_id: Option<u8>,
    playout_delay: Option<u16>,
    payload: Vec<u8>,
}

fn receiver_parse(packet: &[u8], ssrc: u32) -> Option<Parsed> {
    if packet.len() < 18 || packet[0] != 0b1000_0000 {
        return None;
    }
    let payload_type = packet[1] & 0x7f;
    if !matches!(payload_type, 96..=104 | 127) {
        return None;
    }
    if u32::from_be_bytes(packet[8..12].try_into().unwrap()) != ssrc {
        return None;
    }
    let byte12 = packet[12];
    let packet_id = u16::from_be_bytes([packet[14], packet[15]]);
    let max_packet_id = u16::from_be_bytes([packet[16], packet[17]]);
    if max_packet_id == 0xffff || packet_id > max_packet_id {
        return None;
    }
    let mut offset = 18;
    let referenced_frame_id = (byte12 & 0x40 != 0).then(|| {
        offset += 1;
        packet[17 + 1]
    });
    let mut playout_delay = None;
    for _ in 0..(byte12 & 0x3f) {
        let type_size = u16::from_be_bytes([packet[offset], packet[offset + 1]]);
        let size = usize::from(type_size & 0x3ff);
        if type_size >> 10 == 1 && size == 2 {
            playout_delay = Some(u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]));
        }
        offset += 2 + size;
    }
    Some(Parsed {
        payload_type,
        marker: packet[1] & 0x80 != 0,
        sequence: u16::from_be_bytes([packet[2], packet[3]]),
        rtp_timestamp: u32::from_be_bytes(packet[4..8].try_into().unwrap()),
        key_frame: byte12 & 0x80 != 0,
        frame_id: packet[13],
        packet_id,
        max_packet_id,
        referenced_frame_id,
        playout_delay,
        payload: packet[offset..].to_vec(),
    })
}

#[test]
fn test_packets_parse_with_receiver_rules_and_reassemble_the_frame() {
    let data: Vec<u8> = (0..3 * rtp::MAX_PAYLOAD_BYTES + 17)
        .map(|i| i as u8)
        .collect();
    let frame = rtp::EncryptedFrame {
        frame_id: 300,
        referenced_frame_id: 300,
        key_frame: true,
        rtp_timestamp: 0xdead_beef,
        new_playout_delay_ms: Some(400),
        data: data.clone(),
    };
    let mut packetizer = rtp::Packetizer::new(96, 60_000, 0xfffe);
    let count = rtp::packet_count(data.len()).unwrap();
    assert_eq!(count, 4);
    let mut reassembled = Vec::new();
    let mut out = Vec::new();
    for id in 0..count as u16 {
        packetizer.packet(&frame, id, &mut out);
        assert!(out.len() <= rtp::MAX_PACKET_BYTES);
        let parsed = receiver_parse(&out, 60_000).expect("receiver accepts packet");
        assert_eq!(parsed.payload_type, 96);
        assert_eq!(parsed.marker, id == 3);
        assert_eq!(parsed.sequence, 0xfffe_u16.wrapping_add(id));
        assert_eq!(parsed.rtp_timestamp, 0xdead_beef);
        assert!(parsed.key_frame);
        assert_eq!(parsed.frame_id, 300_i64 as u8);
        assert_eq!((parsed.packet_id, parsed.max_packet_id), (id, 3));
        assert_eq!(parsed.referenced_frame_id, Some(300_i64 as u8));
        assert_eq!(parsed.playout_delay, (id == 0).then_some(400));
        reassembled.extend(parsed.payload);
    }
    assert_eq!(reassembled, data);
    assert!(
        receiver_parse(&out, 1).is_none(),
        "wrong SSRC must not parse"
    );
    // A zero-byte frame still produces exactly one packet.
    assert_eq!(rtp::packet_count(0), Some(1));
    assert_eq!(rtp::packet_count(usize::MAX / 2), None);
}

#[test]
fn test_sender_report_layout_and_ntp_report_id() {
    let ntp = rtcp::ntp_timestamp(1_700_000_000, 500_000_000);
    assert_eq!(ntp >> 32, 1_700_000_000 + rtcp::NTP_UNIX_OFFSET);
    assert_eq!(ntp & 0xffff_ffff, 1 << 31);
    assert_eq!(rtcp::report_id(0x0192_a3b4_c5d6_e7f8), 0xa3b4_c5d6);
    let report = rtcp::sender_report(77, ntp, 1234, 10, 999);
    assert_eq!(report[0], 0x80);
    assert_eq!(report[1], 200);
    assert_eq!(u16::from_be_bytes([report[2], report[3]]), 6);
    assert_eq!(u32::from_be_bytes(report[4..8].try_into().unwrap()), 77);
    assert_eq!(u64::from_be_bytes(report[8..16].try_into().unwrap()), ntp);
    assert_eq!(u32::from_be_bytes(report[16..20].try_into().unwrap()), 1234);
}

/// Receiver-format compound RTCP (rtp_defines.h diagrams).
fn feedback_packet(
    receiver: u32,
    sender: u32,
    checkpoint: u8,
    losses: &[(u8, u16, u8)],
    acks: &[u8],
    report_id: Option<u32>,
    pli: bool,
) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(id) = report_id {
        out.extend([0x81, 201, 0, 7]);
        out.extend(receiver.to_be_bytes());
        out.extend(sender.to_be_bytes());
        out.extend([0, 0, 0, 3]); // fraction 0, cumulative lost 3
        out.extend([0; 8]);
        out.extend(id.to_be_bytes());
        out.extend(6553_u32.to_be_bytes()); // ~100 ms held
    }
    let mut body = Vec::new();
    body.extend(receiver.to_be_bytes());
    body.extend(sender.to_be_bytes());
    body.extend(b"CAST");
    body.push(checkpoint);
    body.push(losses.len() as u8);
    body.extend(400_u16.to_be_bytes());
    for (frame, packet, bits) in losses {
        body.push(*frame);
        body.extend(packet.to_be_bytes());
        body.push(*bits);
    }
    if !acks.is_empty() {
        body.extend(b"CST2");
        body.push(1);
        body.push(2);
        body.extend(acks);
    }
    while body.len() % 4 != 0 {
        body.push(0);
    }
    out.extend([0x8f, 206]);
    out.extend(((body.len() / 4) as u16).to_be_bytes());
    out.extend(body);
    if pli {
        out.extend([0x81, 206, 0, 2]);
        out.extend(receiver.to_be_bytes());
        out.extend(sender.to_be_bytes());
    }
    out
}

#[test]
fn test_feedback_parses_checkpoint_nacks_acks_report_and_picture_loss() {
    let packet = feedback_packet(
        5,
        9,
        10,
        &[(12, 3, 0b101), (13, 0xffff, 0)],
        &[0b0000_0011, 0],
        Some(0xabcd),
        true,
    );
    let feedback = rtcp::parse(&packet, 5, 9, 20).unwrap();
    assert_eq!(feedback.checkpoint, Some(10));
    assert_eq!(feedback.playout_delay_ms, 400);
    let nacks: Vec<_> = feedback
        .nacks
        .iter()
        .map(|n| (n.frame_id, n.packet_id))
        .collect();
    assert_eq!(nacks, [(12, 3), (12, 4), (12, 6), (13, 0xffff)]);
    assert_eq!(feedback.acks, [12, 13]);
    assert_eq!(feedback.report.unwrap().last_report_id, 0xabcd);
    assert_eq!(feedback.report.unwrap().cumulative_lost, 3);
    assert!(feedback.picture_loss);
    // 8-bit frame IDs expand relative to the newest frame sent.
    let wrapped = feedback_packet(5, 9, 0xff, &[(0x01, 0, 0)], &[], None, false);
    let feedback = rtcp::parse(&wrapped, 5, 9, 0x101).unwrap();
    assert_eq!(feedback.checkpoint, Some(0xff));
    assert_eq!(feedback.nacks[0].frame_id, 0x101);
    // Other SSRCs are ignored; malformed input is rejected.
    assert_eq!(
        rtcp::parse(&packet, 6, 9, 20).unwrap(),
        rtcp::Feedback::default()
    );
    assert!(rtcp::parse(&packet[..packet.len() - 3], 5, 9, 20).is_none());
    assert!(rtcp::parse(&[0x40, 201, 0, 0], 5, 9, 20).is_none());
}

/// Loopback: a scripted receiver checks SR-before-media, decrypts and
/// reassembles frames, NACKs one packet, ACKs, and sends picture loss.
#[tokio::test]
async fn test_streamer_end_to_end_over_udp_with_nack_retransmit_ack_and_pli() {
    let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sender_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sender_socket
        .connect(receiver.local_addr().unwrap())
        .await
        .unwrap();
    receiver
        .connect(sender_socket.local_addr().unwrap())
        .await
        .unwrap();
    let offer = video(1, 60_000);
    let streamer = Streamer::spawn(
        sender_socket,
        vec![StreamSpec {
            offer: offer.clone(),
            receiver_ssrc: 4242,
        }],
    );
    let stream = streamer.stream(1).unwrap();
    let mut stats = streamer.stats(stream).unwrap();
    let frame: Vec<u8> = (0..2 * rtp::MAX_PAYLOAD_BYTES + 5)
        .map(|i| (i * 7) as u8)
        .collect();
    streamer
        .send(
            stream,
            EncodedFrame {
                key_frame: true,
                rtp_timestamp: 90_000,
                data: frame.clone(),
            },
        )
        .unwrap();

    let crypto = rtp::FrameCrypto::new(offer.aes_key, offer.aes_iv_mask);
    let mut buffer = [0_u8; 2048];
    let mut parts: Vec<Option<Vec<u8>>> = vec![None; 3];
    let mut first_was_report = None;
    let mut report_id = 0;
    timeout(Duration::from_secs(3), async {
        while parts.iter().any(Option::is_none) {
            let n = receiver.recv(&mut buffer).await.unwrap();
            let packet = &buffer[..n];
            if packet[1] == 200 {
                first_was_report.get_or_insert(true);
                report_id = rtcp::report_id(u64::from_be_bytes(packet[8..16].try_into().unwrap()));
                continue;
            }
            first_was_report.get_or_insert(false);
            let parsed = receiver_parse(packet, 60_000).unwrap();
            assert_eq!(parsed.frame_id, 0);
            parts[usize::from(parsed.packet_id)] = Some(parsed.payload);
        }
    })
    .await
    .unwrap();
    assert_eq!(first_was_report, Some(true), "sender report precedes media");
    let mut data: Vec<u8> = parts.into_iter().flatten().flatten().collect();
    crypto.crypt(0, &mut data);
    assert_eq!(data, frame);

    // NACK packet 1 of frame 0 (checkpoint = leader 0xff) plus a report.
    let nack = feedback_packet(
        4242,
        60_000,
        0xff,
        &[(0, 1, 0)],
        &[],
        Some(report_id),
        false,
    );
    receiver.send(&nack).await.unwrap();
    let retransmitted = timeout(Duration::from_secs(3), async {
        loop {
            let n = receiver.recv(&mut buffer).await.unwrap();
            if let Some(parsed) = receiver_parse(&buffer[..n], 60_000) {
                return parsed.packet_id;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(retransmitted, 1);

    // ACK frame 0 and report picture loss.
    receiver
        .send(&feedback_packet(4242, 60_000, 0, &[], &[], None, true))
        .await
        .unwrap();
    let after = timeout(Duration::from_secs(3), async {
        loop {
            stats.changed().await.unwrap();
            let current = stats.borrow_and_update().clone();
            if current.checkpoint == 0 && current.key_frame_requests >= 1 {
                return current;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(after.frames_enqueued, 1);
    assert!(after.packets_retransmitted >= 1);
    assert!(after.round_trip_ms <= 400);
    assert!(after.feedback_received >= 2);
}

#[tokio::test]
async fn test_predicted_frames_are_dropped_when_too_much_is_in_flight() {
    let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket
        .connect(receiver.local_addr().unwrap())
        .await
        .unwrap();
    let streamer = Streamer::spawn(
        socket,
        vec![StreamSpec {
            offer: video(1, 60_000),
            receiver_ssrc: 1,
        }],
    );
    let mut stats = streamer.stats(0).unwrap();
    // Nothing is ever ACKed: 1 s of 30 fps frames exceeds the in-flight window.
    for n in 0..30_u32 {
        let frame = EncodedFrame {
            key_frame: n == 0,
            rtp_timestamp: n * 3000,
            data: vec![0; 100],
        };
        while streamer.send(0, frame.clone()).is_err() {
            tokio::task::yield_now().await;
        }
    }
    let result = timeout(Duration::from_secs(3), async {
        loop {
            let current = stats.borrow_and_update().clone();
            if current.frames_enqueued + current.frames_dropped == 30 {
                return current;
            }
            stats.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(result.frames_dropped > 0);
    assert!(
        result.key_frame_requests >= 1,
        "drops must request a key frame"
    );
    assert!(result.frames_enqueued >= 2);
}
