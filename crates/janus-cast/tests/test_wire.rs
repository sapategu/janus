use bytes::Bytes;
use janus_cast::wire::*;
use prost::Message as _;
fn ping() -> Message {
    Message::new(
        SENDER,
        RECEIVER,
        HEARTBEAT_NAMESPACE,
        Payload::Text("{\"type\":\"PING\"}".into()),
    )
    .unwrap()
}

#[test]
fn proto2_zero_values_have_presence_and_stable_wire_tags() {
    let bytes = encode(&ping()).unwrap();
    assert_eq!(&bytes[..2], &[8, 0]);
    assert!(bytes.windows(2).any(|pair| pair == [40, 0]));
    assert_eq!(decode(&bytes).unwrap(), ping());
}

#[test]
fn missing_required_fields_and_mixed_payloads_fail_closed() {
    let bytes = encode(&ping()).unwrap();
    assert!(matches!(decode(&bytes[2..]), Err(Error::Invalid)));
    let mut envelope = Envelope::decode(bytes.as_slice()).unwrap();
    envelope.binary = Some(Bytes::from_static(b"extra"));
    assert!(matches!(
        decode(&envelope.encode_to_vec()),
        Err(Error::Invalid)
    ));
    envelope.binary = None;
    envelope.continued = Some(true);
    assert!(matches!(
        decode(&envelope.encode_to_vec()),
        Err(Error::Unsupported)
    ));
}

#[test]
fn bounds_include_envelope_overhead_and_identifiers_are_sanitized() {
    let huge = Message::new(
        SENDER,
        RECEIVER,
        AUTH_NAMESPACE,
        Payload::Binary(Bytes::from(vec![0; MAX_FRAME_BYTES])),
    )
    .unwrap();
    assert!(matches!(encode(&huge), Err(Error::Length)));
    assert!(
        Message::new(
            "bad\nsource",
            RECEIVER,
            HEARTBEAT_NAMESPACE,
            Payload::Text("{}".into())
        )
        .is_err()
    );
    assert!(
        Message::new(
            SENDER,
            RECEIVER,
            "wrong-namespace",
            Payload::Text("{}".into())
        )
        .is_err()
    );
}

#[tokio::test]
async fn framed_duplex_and_oversized_prefix_rejection() {
    let (mut client, mut receiver) = tokio::io::duplex(4096);
    let message = ping();
    write(&mut client, &message).await.unwrap();
    assert_eq!(read(&mut receiver).await.unwrap(), message);
    let prefix = (MAX_FRAME_BYTES as u32 + 1).to_be_bytes();
    assert!(matches!(
        read(&mut prefix.as_slice()).await,
        Err(Error::Length)
    ));
}
