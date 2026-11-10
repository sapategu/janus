use janus_session::protocol::*;
use janus_session::{Coordinator, Grant, Source};
use janus_wire::{Code, Envelope, Mode, Payload};
use janus_wire::{Empty, Hello, Offer};

fn request(n: u64, payload: Payload, grant: Option<Grant>) -> Envelope {
    Envelope {
        request_id: n,
        sequence: n,
        session_id: grant.map(|g| g.session.to_vec()).unwrap_or_default(),
        generation: grant.map(|g| g.generation).unwrap_or_default(),
        payload: Some(payload),
        ..Default::default()
    }
}
fn hello(peer: &mut ControlPeer, coordinator: &mut Coordinator) {
    assert_eq!(
        status(
            &peer
                .dispatch(
                    coordinator,
                    request(1, Payload::Hello(Hello { major: 1 }), None),
                    0
                )
                .unwrap()
        ),
        Code::Ok
    );
}
fn offer(peer: &mut ControlPeer, coordinator: &mut Coordinator, n: u64) -> Envelope {
    let mut request = request(
        n,
        Payload::Offer(Offer {
            mode: Mode::Synthetic as i32,
        }),
        None,
    );
    request.session_id = vec![n as u8; 16];
    peer.dispatch(coordinator, request, coordinator.now())
        .unwrap()
}
fn status(response: &Envelope) -> Code {
    let Some(Payload::Status(status)) = &response.payload else {
        panic!("missing status")
    };
    Code::try_from(status.code).unwrap()
}
fn receiver() -> Coordinator {
    let mut c = Coordinator::new(vec![Source::Synthetic]);
    c.set_receiving(true);
    c
}

#[test]
fn connections_cannot_steal_grants_and_stale_stop_is_idempotent() {
    let mut c = receiver();
    let mut owner = ControlPeer::default();
    let mut other = ControlPeer::default();
    hello(&mut owner, &mut c);
    hello(&mut other, &mut c);
    assert_eq!(status(&offer(&mut owner, &mut c, 2)), Code::Ok);
    let grant = owner.owned().unwrap();
    assert_eq!(status(&offer(&mut other, &mut c, 2)), Code::Busy);
    assert_eq!(
        status(
            &other
                .dispatch(&mut c, request(3, Payload::Stop(Empty {}), Some(grant)), 0)
                .unwrap()
        ),
        Code::Stale
    );
    let stop = request(3, Payload::Stop(Empty {}), Some(grant));
    let response = owner.dispatch(&mut c, stop.clone(), 0).unwrap();
    assert_eq!(status(&response), Code::Ok);
    assert_eq!(owner.dispatch(&mut c, stop, 1).unwrap(), response);
    assert_eq!(status(&offer(&mut other, &mut c, 4)), Code::Ok);
    assert_eq!(
        status(
            &owner
                .dispatch(&mut c, request(4, Payload::Stop(Empty {}), Some(grant)), 1)
                .unwrap()
        ),
        Code::Ok
    );
    assert!(c.snapshot().output_permitted);
}

#[test]
fn replay_cannot_renew_lease_or_mutate_previous_request() {
    let mut c = receiver();
    let mut peer = ControlPeer::default();
    hello(&mut peer, &mut c);
    offer(&mut peer, &mut c, 2);
    let grant = peer.owned().unwrap();
    let heartbeat = request(3, Payload::Heartbeat(Empty {}), Some(grant));
    peer.dispatch(&mut c, heartbeat.clone(), 100).unwrap();
    peer.dispatch(&mut c, heartbeat.clone(), 1_000_000).unwrap();
    c.tick(2_000_100).unwrap();
    assert!(!c.snapshot().output_permitted);
    let mut different = heartbeat;
    different.payload = Some(Payload::Stop(Empty {}));
    assert!(peer.dispatch(&mut c, different, 2_000_101).is_err());
}

#[test]
fn missing_hello_version_mismatch_flood_and_disconnect() {
    let mut c = receiver();
    let mut peer = ControlPeer::default();
    assert_eq!(status(&offer(&mut peer, &mut c, 1)), Code::Invalid);
    let response = peer
        .dispatch(
            &mut c,
            request(2, Payload::Hello(Hello { major: 99 }), None),
            0,
        )
        .unwrap();
    assert_eq!(status(&response), Code::Unsupported);
    let response = peer
        .dispatch(
            &mut c,
            request(3, Payload::Hello(Hello { major: 1 }), None),
            0,
        )
        .unwrap();
    assert_eq!(status(&response), Code::Ok);
    assert_eq!(status(&offer(&mut peer, &mut c, 4)), Code::Ok);
    let grant = peer.owned().unwrap();
    for n in 5..=16 {
        peer.dispatch(
            &mut c,
            request(n, Payload::Heartbeat(Empty {}), Some(grant)),
            0,
        )
        .unwrap();
    }
    assert!(
        peer.dispatch(
            &mut c,
            request(17, Payload::Heartbeat(Empty {}), Some(grant)),
            0
        )
        .is_err()
    );
    peer.disconnect(&mut c);
    assert!(!c.snapshot().output_permitted);
}

#[tokio::test]
async fn protobuf_duplex_runs_full_protocol_without_network_permissions() {
    let (mut sender, mut receiver_io) = tokio::io::duplex(65536);
    let server = tokio::spawn(async move {
        let mut coordinator = receiver();
        let mut peer = ControlPeer::default();
        for _ in 0..4 {
            let request = janus_wire::read(&mut receiver_io).await.unwrap();
            let response = peer.dispatch(&mut coordinator, request, 0).unwrap();
            janus_wire::write(&mut receiver_io, &response)
                .await
                .unwrap();
        }
        assert!(!coordinator.snapshot().output_permitted);
    });
    janus_wire::write(
        &mut sender,
        &request(1, Payload::Hello(Hello { major: 1 }), None),
    )
    .await
    .unwrap();
    assert_eq!(
        status(&janus_wire::read(&mut sender).await.unwrap()),
        Code::Ok
    );
    let mut offer = request(
        2,
        Payload::Offer(Offer {
            mode: Mode::Synthetic as i32,
        }),
        None,
    );
    offer.session_id = vec![2; 16];
    janus_wire::write(&mut sender, &offer).await.unwrap();
    let response = janus_wire::read(&mut sender).await.unwrap();
    let grant = Grant {
        session: [2; 16],
        generation: response.generation,
        source: Source::Synthetic,
    };
    for (n, payload) in [
        (3, Payload::Heartbeat(Empty {})),
        (4, Payload::Stop(Empty {})),
    ] {
        janus_wire::write(&mut sender, &request(n, payload, Some(grant)))
            .await
            .unwrap();
        assert_eq!(
            status(&janus_wire::read(&mut sender).await.unwrap()),
            Code::Ok
        );
    }
    server.await.unwrap();
}
