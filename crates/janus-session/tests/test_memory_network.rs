mod support;
use quinn::{AsyncUdpSocket, udp::Transmit};
use std::{
    sync::atomic::Ordering,
    sync::{Arc, Mutex},
};
use support::memory_network::*;

#[tokio::test]
async fn stalled_consumer_cannot_grow_packet_queue_and_drop_releases_payloads() {
    let network = Arc::new(Network::impaired());
    let source = MemorySocket {
        address: "127.0.0.1:45835".parse().unwrap(),
        network: network.clone(),
        inbox: Mutex::default(),
    };
    let target = Arc::new(MemorySocket {
        address: "127.0.0.1:45836".parse().unwrap(),
        network: network.clone(),
        inbox: Mutex::default(),
    });
    network
        .routes
        .lock()
        .unwrap()
        .insert(target.address, Arc::downgrade(&target));
    let payload = [0; 1200];
    for _ in 0..10_000 {
        source
            .try_send(&Transmit {
                destination: target.address,
                ecn: None,
                contents: &payload,
                segment_size: None,
                src_ip: None,
            })
            .unwrap();
    }
    assert!(network.counters().2 > 0);
    assert_eq!(target.inbox.lock().unwrap().packets.len(), MAX_PACKETS);
    assert_eq!(
        network.queued_bytes.load(Ordering::Relaxed),
        MAX_PACKETS * payload.len()
    );
    assert_eq!(network.counters().3, MAX_PACKETS * payload.len());
    drop(target);
    assert_eq!(network.queued_bytes.load(Ordering::Relaxed), 0);
}
