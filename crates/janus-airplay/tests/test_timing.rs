use janus_airplay::testing;

#[test]
fn test_timing_reply_matches_receiver_request_and_monotonic_clock() {
    let mut request = [0x5a; 40];
    request[..4].copy_from_slice(&[0x80, 0xd2, 0, 7]);
    let sent = 0x1122334455667788u64;
    request[24..32].copy_from_slice(&sent.to_be_bytes());
    let now = (2208988800u64 << 32) | 0x80000000;
    for length in [32, 40] {
        let reply = testing::timing_reply(&request[..length], now).unwrap();
        assert_eq!(&reply[..4], &[0x80, 0xd3, 0, 7]);
        assert_eq!(&reply[4..8], &request[4..8]);
        assert_eq!(&reply[8..16], &sent.to_be_bytes());
        assert_eq!(&reply[16..24], &now.to_be_bytes());
        assert_eq!(&reply[24..32], &now.to_be_bytes());
    }
}

#[test]
fn test_timing_never_answers_responses_or_invalid_requests() {
    let mut valid = vec![0; 32];
    valid[..2].copy_from_slice(&[0x80, 0xd2]);
    let mut response = valid.clone();
    response[1] = 0xd3;
    let mut wrong_version = valid.clone();
    wrong_version[0] = 0x90;
    let mut wrong_type = valid.clone();
    wrong_type[1] = 0xd4;
    let cases = [valid[..31].to_vec(), response, wrong_version, wrong_type];
    assert_eq!(cases.len(), 4);
    for packet in cases {
        assert!(testing::timing_reply(&packet, 0).is_none());
    }
}
