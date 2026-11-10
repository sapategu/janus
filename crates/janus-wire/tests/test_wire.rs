use janus_wire::*;
use prost::Message as _;

#[test]
fn hello_golden_vector() {
    let message = Envelope {
        request_id: 1,
        sequence: 1,
        payload: Some(Payload::Hello(Hello { major: 1 })),
        ..Default::default()
    };
    let golden = [0x08, 0x01, 0x20, 0x01, 0x52, 0x02, 0x08, 0x01];
    assert_eq!(message.encode_to_vec(), golden);
    assert_eq!(decode(&golden).unwrap(), message);
}

#[tokio::test]
async fn rejects_oversized_prefix_before_reading_body() {
    let bytes = ((MAX_MESSAGE_BYTES + 1) as u32).to_be_bytes();
    assert!(matches!(
        read(&mut bytes.as_slice()).await,
        Err(WireError::Length)
    ));
}

#[test]
fn rejects_missing_payload_bad_session_and_truncation() {
    assert!(decode(&[8, 1, 32, 1]).is_err());
    assert!(decode(&[0x52, 0xff]).is_err());
    let message = Envelope {
        request_id: 1,
        sequence: 1,
        session_id: vec![1; 17],
        payload: Some(Payload::Stop(Empty {})),
        ..Default::default()
    };
    assert!(message.validate().is_err());
}
