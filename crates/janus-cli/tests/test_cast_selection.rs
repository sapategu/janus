use janus_cli::cast::choose;
use janus_discovery::{AdvertisedOutputs, Availability, Candidate, ReceiverProtocol};
fn candidate(protocol: ReceiverProtocol, key: &str) -> Candidate {
    Candidate {
        key: key.into(),
        protocol,
        instance: key.into(),
        advertised_id: None,
        display_name: "Screen".into(),
        model: None,
        endpoints: vec!["10.1.1.2:7000".parse().unwrap()],
        availability: Availability::Unknown,
        advertised_outputs: AdvertisedOutputs {
            video: Some(true),
            audio: Some(true),
        },
    }
}
#[test]
fn test_each_supported_protocol_uses_its_candidate_key() {
    for protocol in [ReceiverProtocol::GoogleCast, ReceiverProtocol::AirPlay] {
        let original = candidate(protocol, "browse-key");
        assert_eq!(
            choose(std::slice::from_ref(&original), Some("Screen"))
                .unwrap()
                .key,
            original.key
        );
        assert_eq!(choose(&[original], None).unwrap().protocol, protocol);
    }
}
#[test]
fn test_duplicate_names_never_select_by_list_position() {
    let cast = candidate(ReceiverProtocol::GoogleCast, "cast-key");
    let airplay = candidate(ReceiverProtocol::AirPlay, "airplay-key");
    for rows in [vec![cast.clone(), airplay.clone()], vec![airplay, cast]] {
        assert!(choose(&rows, Some("Screen")).is_err());
        assert!(choose(&rows, None).is_err());
    }
}
