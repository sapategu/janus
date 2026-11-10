use janus_discovery::{Availability, ReceiverProtocol, parse_hisense_broadcast};
use std::net::SocketAddr;

#[test]
fn parses_hisense_516pro_udp_broadcast() {
    let source: SocketAddr = "192.168.200.163:48689".parse().expect("valid addr");
    let payload = b"Hisense516Pro:20240514:Azuba-Board:8000:51010:sn=0x1001bf:";
    let candidate = parse_hisense_broadcast(payload, source).expect("valid Hisense packet");
    assert_eq!(candidate.display_name, "Azuba-Board");
    assert_eq!(candidate.protocol, ReceiverProtocol::AirPlay);
    assert_eq!(
        candidate.endpoints,
        vec!["192.168.200.163:51010".parse().expect("valid endpoint")]
    );
    assert_eq!(candidate.advertised_id.as_deref(), Some("0x1001bf"));
    assert_eq!(candidate.availability, Availability::AdvertisedReady);
    assert_eq!(candidate.advertised_outputs.video, Some(true));
}

#[test]
fn parses_ecloudbox_binary_broadcast_and_rejects_public_ips() {
    let mut packet = [0u8; 50];
    packet[..9].copy_from_slice(b"ECloudBox");
    packet[12..25].copy_from_slice(b"Board-Room-01");
    let lan_source: SocketAddr = "10.10.0.42:48689".parse().expect("valid addr");
    let candidate = parse_hisense_broadcast(&packet, lan_source).expect("valid binary packet");
    assert_eq!(candidate.display_name, "Board-Room-01");
    assert_eq!(
        candidate.endpoints,
        vec!["10.10.0.42:51010".parse().expect("valid endpoint")]
    );

    let public_source: SocketAddr = "8.8.8.8:48689".parse().expect("valid addr");
    assert!(parse_hisense_broadcast(&packet, public_source).is_none());
}
