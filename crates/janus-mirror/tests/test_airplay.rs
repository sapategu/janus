//! Actual H.264 test-pattern output → AirPlay framing/encryption → independent
//! receiver unframing/decryption → OpenH264 decoding. No physical-display claim.
use aes::{
    Aes128,
    cipher::{KeyIvInit, StreamCipher},
};
use ctr::Ctr128BE;
use janus_encode::{
    h264::{H264Encoder, VideoConfig},
    pattern,
};
use openh264::decoder::Decoder;
use ring::digest;
fn derived(label: &str) -> [u8; 16] {
    let mut data = label.as_bytes().to_vec();
    data.extend_from_slice(&[3; 16]);
    digest::digest(&digest::SHA512, &data).as_ref()[..16]
        .try_into()
        .unwrap()
}
#[test]
fn test_airplay_test_pattern_decodes_at_receiver() {
    let mut encoder = H264Encoder::new(VideoConfig {
        width: 1280,
        height: 720,
        frame_rate: 30,
        bit_rate: 3_000_000,
    })
    .unwrap();
    let mut bgra = Vec::new();
    let mut frames = Vec::new();
    for n in 0..8 {
        pattern::draw(n, 30, 1280, 720, &mut bgra);
        if let Some(frame) = encoder.encode(&bgra, n == 0).unwrap() {
            frames.push((frame.data, frame.key_frame));
        }
    }
    assert!(frames.len() >= 4);
    for http in [false, true] {
        let packets = if http {
            janus_airplay::testing::http_frames([3; 16], [7; 16], frames.clone()).unwrap()
        } else {
            janus_airplay::testing::framed_sequence([3; 16], 123, frames.clone()).unwrap()
        };
        assert!(packets.len() >= 5);
        let (key, iv) = if http {
            ([3; 16], [7; 16])
        } else {
            (
                derived("AirPlayStreamKey123"),
                derived("AirPlayStreamIV123"),
            )
        };
        let mut cipher = Ctr128BE::<Aes128>::new(&key.into(), &iv.into());
        let mut decoder = Decoder::new().unwrap();
        let mut decoded = 0;
        for packet in packets {
            assert_eq!(
                u32::from_le_bytes(packet[..4].try_into().unwrap()) as usize,
                packet.len() - 128
            );
            let mut annex = Vec::new();
            let mut payload = packet[128..].to_vec();
            if packet[4] == 1 {
                assert_eq!(packet[6], if http { 6 } else { 0x16 });
                let sps_len = u16::from_be_bytes(payload[6..8].try_into().unwrap()) as usize;
                annex.extend_from_slice(&[0, 0, 0, 1]);
                annex.extend_from_slice(&payload[8..8 + sps_len]);
                let at = 8 + sps_len;
                assert_eq!(payload[at], 1);
                let pps_len =
                    u16::from_be_bytes(payload[at + 1..at + 3].try_into().unwrap()) as usize;
                annex.extend_from_slice(&[0, 0, 0, 1]);
                annex.extend_from_slice(&payload[at + 3..at + 3 + pps_len]);
            } else {
                assert_eq!(packet[4], 0);
                cipher.apply_keystream(&mut payload);
                let mut at = 0;
                while at < payload.len() {
                    let len = u32::from_be_bytes(payload[at..at + 4].try_into().unwrap()) as usize;
                    at += 4;
                    annex.extend_from_slice(&[0, 0, 0, 1]);
                    annex.extend_from_slice(&payload[at..at + len]);
                    at += len;
                }
            }
            if decoder.decode(&annex).unwrap().is_some() {
                decoded += 1;
            }
        }
        assert!(
            decoded >= 3,
            "independent receiver decoded {decoded} frames"
        );
    }
}
