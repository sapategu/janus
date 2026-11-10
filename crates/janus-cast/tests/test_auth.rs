use bytes::Bytes;
use janus_cast::auth::*;
use janus_cast::wire::{AUTH_NAMESPACE, Message, Payload, RECEIVER, SENDER};
use prost::Message as _;
use std::time::Instant;
fn response(challenge: &Challenge) -> Message {
    let auth = DeviceAuth {
        challenge: None,
        error: None,
        response: Some(AuthResponse {
            signature: Some(vec![1; 256]),
            certificate: Some(b"test-device-cert".to_vec()),
            intermediates: vec![],
            signature_algorithm: Some(1),
            sender_nonce: Some(challenge.nonce().to_vec()),
            hash_algorithm: Some(1),
            crl: None,
        }),
    };
    Message::new(
        RECEIVER,
        SENDER,
        AUTH_NAMESPACE,
        Payload::Binary(Bytes::from(auth.encode_to_vec())),
    )
    .unwrap()
}

#[test]
fn fresh_challenge_uses_strong_hash_and_nonce() {
    let first = Challenge::new(b"tls-cert", Instant::now()).unwrap();
    let second = Challenge::new(b"tls-cert", Instant::now()).unwrap();
    assert_ne!(first.nonce(), second.nonce());
    let message = first.message().unwrap();
    let Payload::Binary(bytes) = message.payload else {
        panic!("wrong payload")
    };
    let auth = DeviceAuth::decode(bytes.as_ref())
        .unwrap()
        .challenge
        .unwrap();
    assert_eq!(auth.sender_nonce.as_deref(), Some(first.nonce().as_slice()));
    assert_eq!(auth.hash_algorithm, Some(1));
}

#[test]
fn parsed_inputs_are_bound_to_nonce_and_this_tls_certificate_but_not_trusted() {
    let challenge = Challenge::new(b"this-channel-cert", Instant::now()).unwrap();
    let nonce = challenge.nonce();
    let reply = response(&challenge);
    let credentials = challenge.parse_response(&reply, Instant::now()).unwrap();
    assert_eq!(&credentials.signed_input()[..NONCE_BYTES], nonce.as_slice());
    assert_eq!(
        &credentials.signed_input()[NONCE_BYTES..],
        b"this-channel-cert"
    );
    assert!(!format!("{credentials:?}").contains("test-device-cert"));
    // Synthetic credentials above are structurally acceptable, not a valid
    // certificate or signature. There is deliberately no auth-success API.
}

#[test]
fn another_challenge_and_expired_response_are_rejected() {
    let now = Instant::now();
    let first = Challenge::new(b"tls-cert", now).unwrap();
    let second = Challenge::new(b"tls-cert", now).unwrap();
    let reply = response(&first);
    assert!(matches!(
        second.parse_response(&reply, now),
        Err(Error::Nonce)
    ));
    assert!(matches!(
        first.parse_response(&reply, now + AUTH_TIMEOUT),
        Err(Error::Expired)
    ));
}

#[test]
fn legacy_hash_missing_nonce_and_wrong_routing_fail_closed() {
    for mutation in 0..3 {
        let challenge = Challenge::new(b"tls-cert", Instant::now()).unwrap();
        let mut reply = response(&challenge);
        let Payload::Binary(bytes) = &reply.payload else {
            unreachable!()
        };
        let mut auth = DeviceAuth::decode(bytes.as_ref()).unwrap();
        match mutation {
            0 => auth.response.as_mut().unwrap().hash_algorithm = None,
            1 => auth.response.as_mut().unwrap().sender_nonce = None,
            _ => reply.source = "wrong-device".into(),
        }
        reply.payload = Payload::Binary(Bytes::from(auth.encode_to_vec()));
        assert!(challenge.parse_response(&reply, Instant::now()).is_err());
    }
}

#[test]
fn contradictory_auth_payload_and_certificate_flood_are_rejected() {
    for mutation in 0..2 {
        let challenge = Challenge::new(b"tls-cert", Instant::now()).unwrap();
        let mut reply = response(&challenge);
        let Payload::Binary(bytes) = &reply.payload else {
            unreachable!()
        };
        let mut auth = DeviceAuth::decode(bytes.as_ref()).unwrap();
        if mutation == 0 {
            auth.error = Some(AuthError { kind: Some(0) });
        } else {
            auth.response.as_mut().unwrap().intermediates = vec![vec![1]; MAX_INTERMEDIATES + 1];
        }
        reply.payload = Payload::Binary(Bytes::from(auth.encode_to_vec()));
        assert!(challenge.parse_response(&reply, Instant::now()).is_err());
    }
}
