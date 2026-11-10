use hickory_proto::rr::RData;
use janus_discovery::*;
use std::net::{Ipv4Addr, SocketAddrV4};

fn address() -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 10), 45831)
}
#[test]
fn advertisement_roundtrip_expiry_and_goodbye() {
    let mut cache = Cache::default();
    let bytes = advertisement("Room", address(), true, 120).unwrap();
    cache.ingest(&bytes, 0).unwrap();
    let candidates = cache.candidates(1);
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].endpoints,
        vec![std::net::SocketAddr::V4(address())]
    );
    assert_eq!(candidates[0].availability, Availability::AdvertisedReady);
    cache
        .ingest(&advertisement("Room", address(), false, 120).unwrap(), 10)
        .unwrap();
    assert_eq!(
        cache.candidates(11)[0].availability,
        Availability::AdvertisedBusy
    );
    cache
        .ingest(&advertisement("Room", address(), false, 0).unwrap(), 20)
        .unwrap();
    assert!(cache.candidates(21).is_empty());
    cache.ingest(&bytes, 30).unwrap();
    assert!(cache.candidates(120_030).is_empty());
}
#[test]
fn public_endpoint_and_wrong_protocol_are_not_candidates() {
    let mut cache = Cache::default();
    cache
        .ingest(
            &advertisement(
                "Room",
                SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 45831),
                true,
                120,
            )
            .unwrap(),
            0,
        )
        .unwrap();
    assert!(cache.candidates(0).is_empty());
    let mut message = packet(&advertisement("Local", address(), true, 120).unwrap()).unwrap();
    message
        .additionals
        .retain(|r| !matches!(&r.data, RData::TXT(_)));
    cache.ingest(&message.to_vec().unwrap(), 0).unwrap();
    assert!(cache.candidates(0).is_empty());
}
#[test]
fn malformed_counts_and_candidate_flood_are_bounded() {
    let mut cache = Cache::default();
    let mut bytes = [0; 12];
    bytes[6] = 255;
    bytes[7] = 255;
    assert!(cache.ingest(&bytes, 0).is_err());
    assert!(cache.ingest(&[0; 2], 0).is_err());
    for n in 0..1000 {
        cache
            .ingest(
                &advertisement(&format!("room-{n}"), address(), true, 120).unwrap(),
                0,
            )
            .unwrap();
    }
    assert!(cache.record_count() <= 512);
    assert!(cache.candidates(0).len() <= MAX_CANDIDATES);
}
