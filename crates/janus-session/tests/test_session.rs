use janus_session::*;
fn receiver() -> Coordinator {
    let mut c = Coordinator::new(vec![Source::Synthetic]);
    c.set_receiving(true);
    c
}

#[test]
fn receive_off_and_native_source_fail_closed() {
    let mut c = Coordinator::new(vec![Source::Synthetic]);
    assert_eq!(
        c.offer([1; 16], Source::Synthetic, 1, 0),
        Err(SessionError::Disabled)
    );
    c.set_receiving(true);
    assert_eq!(
        c.offer([1; 16], Source::Video, 1, 0),
        Err(SessionError::Unsupported)
    );
    assert!(!c.snapshot().output_permitted);
}

#[test]
fn lease_expiry_requires_new_generation_and_old_work_stays_revoked() {
    let mut c = receiver();
    let g = c.offer([1; 16], Source::Synthetic, 1, 0).unwrap();
    c.started(g, 1).unwrap();
    assert!(c.allows(g, OUTPUT_LEASE_US - 1));
    assert!(!c.allows(g, OUTPUT_LEASE_US));
    assert_eq!(
        c.heartbeat(g, 2, OUTPUT_LEASE_US),
        Err(SessionError::Expired)
    );
    let next = c.resume(g, 3, OUTPUT_LEASE_US + 1).unwrap();
    assert_ne!(g.generation, next.generation);
    c.started(next, OUTPUT_LEASE_US + 2).unwrap();
    assert!(!c.allows(g, OUTPUT_LEASE_US + 3));
    assert!(c.allows(next, OUTPUT_LEASE_US + 3));
}

#[test]
fn stop_wins_over_recovery_and_competing_senders() {
    let mut c = receiver();
    let g = c.offer([1; 16], Source::Synthetic, 1, 0).unwrap();
    assert_eq!(
        c.offer([2; 16], Source::Synthetic, 1, 0),
        Err(SessionError::Busy)
    );
    c.local_stop();
    assert_eq!(c.resume(g, 2, 1), Err(SessionError::Stale));
    assert_eq!(c.started(g, 2), Err(SessionError::Stale));
    assert!(!c.snapshot().receiving);
}

#[test]
fn duplicate_heartbeats_do_not_extend_lease() {
    let mut c = receiver();
    let g = c.offer([1; 16], Source::Synthetic, 1, 0).unwrap();
    c.started(g, 0).unwrap();
    c.heartbeat(g, 2, 500_000).unwrap();
    assert_eq!(c.heartbeat(g, 2, 1_000_000), Err(SessionError::Stale));
    assert!(!c.allows(g, 2_500_000));
    c.tick(10_500_000).unwrap();
    assert_eq!(c.snapshot().phase, Phase::Idle);
}

#[test]
fn stale_stop_cannot_end_new_session_or_time_go_backwards() {
    let mut c = receiver();
    let old = c.offer([1; 16], Source::Synthetic, 1, 10).unwrap();
    c.remote_stop(old).unwrap();
    let next = c.offer([2; 16], Source::Synthetic, 1, 11).unwrap();
    assert_eq!(c.remote_stop(old), Err(SessionError::Stale));
    assert_eq!(c.tick(9), Err(SessionError::Invalid));
    c.started(next, 12).unwrap();
    assert!(c.allows(next, 13));
}
