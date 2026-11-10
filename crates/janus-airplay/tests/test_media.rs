use aes::{
    Aes128,
    cipher::{BlockDecrypt, KeyInit, KeyIvInit, StreamCipher},
};
use ctr::Ctr128BE;
use janus_airplay::{alac, testing};
use ring::digest;
fn derived(label: &str, key: &[u8]) -> [u8; 16] {
    let mut data = label.as_bytes().to_vec();
    data.extend_from_slice(key);
    digest::digest(&digest::SHA512, &data).as_ref()[..16]
        .try_into()
        .unwrap()
}
#[test]
fn test_video_receiver_rules_and_cipher_continuity() {
    let frames = vec![
        (
            vec![
                0, 0, 0, 1, 0x67, 0x42, 0, 0x1f, 0x80, 0, 0, 1, 0x68, 0xce, 0x80, 0, 0, 1, 0x65,
                0xaa, 0xbb,
            ],
            true,
        ),
        (vec![0, 0, 1, 0x41, 0xcc, 0xdd, 0xee], false),
    ];
    let packets = testing::framed_sequence([3; 16], 123, frames).unwrap();
    assert_eq!(packets.len(), 3);
    let config = &packets[0];
    assert_eq!(config[4..8], [1, 0, 0x16, 1]);
    assert_eq!(config[128..134], [1, 0x42, 0, 0x1f, 0xff, 0xe1]);
    for offset in [16, 40, 56] {
        assert_eq!(
            f32::from_le_bytes(config[offset..offset + 4].try_into().unwrap()),
            1280.
        );
    }
    let mut cipher = Ctr128BE::<Aes128>::new(
        &derived("AirPlayStreamKey123", &[3; 16]).into(),
        &derived("AirPlayStreamIV123", &[3; 16]).into(),
    );
    for (packet, want, key) in [
        (&packets[1], vec![0, 0, 0, 3, 0x65, 0xaa, 0xbb], true),
        (&packets[2], vec![0, 0, 0, 4, 0x41, 0xcc, 0xdd, 0xee], false),
    ] {
        assert_eq!(
            u32::from_le_bytes(packet[..4].try_into().unwrap()) as usize,
            packet.len() - 128
        );
        assert_eq!(packet[4], 0);
        assert_eq!(packet[5], if key { 0x10 } else { 0 });
        let mut decoded = packet[128..].to_vec();
        assert_ne!(decoded, want);
        cipher.apply_keystream(&mut decoded);
        assert_eq!(decoded, want);
    }
    for bad in [
        vec![],
        vec![0, 0, 1],
        vec![0, 0, 1, 0x80],
        vec![0; 4 * 1024 * 1024 + 1],
    ] {
        assert!(testing::framed_vector([3; 16], 123, &bad).is_err());
    }
}
#[test]
fn test_alac_escape_packet_is_lossless_and_rtp_cbc_matches_receiver() {
    let samples = (0..704)
        .map(|i| (i * 71 - 25000) as i16)
        .collect::<Vec<_>>();
    let encoded = alac(&samples).unwrap();
    let mut bit = 0usize;
    let mut read = |n: usize| {
        let mut value = 0u32;
        for _ in 0..n {
            value = (value << 1) | u32::from((encoded[bit / 8] >> (7 - bit % 8)) & 1);
            bit += 1;
        }
        value
    };
    assert_eq!(read(3), 1);
    assert_eq!(read(4), 0);
    assert_eq!(read(12), 0);
    assert_eq!(read(4), 1);
    for sample in &samples {
        assert_eq!(read(16) as u16 as i16, *sample);
    }
    assert_eq!(read(3), 7);
    let packet = testing::audio_packet([5; 16], [7; 16], &samples).unwrap();
    assert_eq!(
        &packet[..12],
        &[0x80, 96, 0, 1, 0, 0, 4, 0xd2, 0x12, 0x34, 0x56, 0x78]
    );
    let aes = Aes128::new(&[5; 16].into());
    let mut chain = [7; 16];
    let mut decoded = packet[12..].to_vec();
    for bytes in decoded.chunks_exact_mut(16) {
        let previous: [u8; 16] = bytes.try_into().unwrap();
        let mut block = aes::cipher::Block::<Aes128>::clone_from_slice(bytes);
        aes.decrypt_block(&mut block);
        for i in 0..16 {
            bytes[i] = block[i] ^ chain[i];
        }
        chain = previous;
    }
    assert_eq!(decoded, encoded);
    assert!(packet.len() <= 1472);
    assert!(alac(&samples[..703]).is_err());
}

#[test]
fn test_audio_retransmissions_are_identical_bounded_and_expire() {
    let packets = (0u16..300)
        .map(|seq| {
            let mut p = vec![0x80, 96];
            p.extend_from_slice(&seq.to_be_bytes());
            p.resize(12, 0x35);
            p
        })
        .collect::<Vec<_>>();
    let request = vec![0x80, 0xd5, 0x12, 0x34, 0, 44, 0xff, 0xff];
    let responses = testing::retransmission_vectors(packets.clone(), vec![request; 5]).unwrap();
    assert_eq!(
        responses.iter().map(Vec::len).collect::<Vec<_>>(),
        [32, 32, 32, 32, 0]
    );
    assert_eq!(&responses[0][0][..4], &[0x80, 0xd6, 0x12, 0x34]);
    assert_eq!(&responses[0][0][4..], packets[44]);
    let expired =
        testing::retransmission_vectors(packets, vec![vec![0x80, 0xd5, 0, 9, 0, 0, 0, 1]]).unwrap();
    assert_eq!(expired[0], [vec![0x80, 0xd6, 0, 9, 0, 0, 0, 0]]);
}
