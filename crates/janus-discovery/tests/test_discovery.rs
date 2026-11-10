//! Synthetic DNS-SD fixtures: AzubaTV is a hardware acceptance target, never a
//! production name match. These bytes are not a capture from the real TV.
use anyhow::Result;
use hickory_proto::{
    op::{Message, OpCode},
    rr::{
        Name, RData, Record,
        rdata::{A, PTR, SRV, TXT},
    },
};
use janus_discovery::*;
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};

fn cast(label: &str, id: &str, name: &str, capabilities: &str, ttl: u32) -> Message {
    let service = Name::from_ascii(GOOGLE_CAST_SERVICE).unwrap();
    let instance = Name::from_ascii(format!("{label}.{GOOGLE_CAST_SERVICE}")).unwrap();
    let host = Name::from_ascii(format!("{label}.local.")).unwrap();
    let mut message = Message::response(0, OpCode::Query);
    message.add_answer(Record::from_rdata(
        service,
        ttl,
        RData::PTR(PTR(instance.clone())),
    ));
    message.add_additional(Record::from_rdata(
        instance.clone(),
        ttl,
        RData::SRV(SRV::new(0, 0, 8009, host.clone())),
    ));
    message.add_additional(Record::from_rdata(
        instance,
        ttl,
        RData::TXT(TXT::new(vec![
            format!("id={id}"),
            format!("fn={name}"),
            "md=Fixture receiver".into(),
            format!("ca={capabilities}"),
        ])),
    ));
    message.add_additional(Record::from_rdata(
        host,
        ttl,
        RData::A(A(Ipv4Addr::new(192, 168, 1, 20))),
    ));
    message
}

#[test]
fn query_searches_existing_google_cast_and_optional_janus_receivers() {
    let query = packet(&query().unwrap()).unwrap();
    assert!(
        query
            .queries
            .iter()
            .any(|query| *query.name() == Name::from_ascii(GOOGLE_CAST_SERVICE).unwrap())
    );
    assert!(
        query
            .queries
            .iter()
            .any(|query| *query.name() == Name::from_ascii(SERVICE).unwrap())
    );
}

#[test]
fn azubatv_and_other_brands_use_the_same_protocol_path() {
    let mut cache = Cache::default();
    for (label, name) in [
        ("device-a", "AzubaTV"),
        ("device-b", "Kitchen display"),
        ("device-c", "Meeting room"),
    ] {
        cache
            .ingest(&cast(label, label, name, "5", 120).to_vec().unwrap(), 0)
            .unwrap();
    }
    let devices = cache.candidates(0);
    assert_eq!(devices.len(), 3);
    assert_eq!(devices[0].display_name, "AzubaTV");
    for device in devices {
        assert_eq!(device.protocol, ReceiverProtocol::GoogleCast);
        assert_eq!(device.availability, Availability::Unknown);
        assert_eq!(
            device.advertised_outputs,
            AdvertisedOutputs {
                video: Some(true),
                audio: Some(true)
            }
        );
        assert_eq!(device.endpoints[0].port(), 8009);
        assert_eq!(device.model.as_deref(), Some("Fixture receiver"));
    }
}

#[test]
fn audio_only_and_malformed_capabilities_do_not_claim_screen_support() {
    let mut cache = Cache::default();
    cache
        .ingest(
            &cast("speaker", "speaker", "Speaker", "4", 120)
                .to_vec()
                .unwrap(),
            0,
        )
        .unwrap();
    let device = cache.candidates(0).remove(0);
    assert!(!device.advertised_outputs.may_support(OutputKind::Video));
    assert!(device.advertised_outputs.may_support(OutputKind::Audio));
    for invalid in ["", "+5", "-1", "18446744073709551616", "video"] {
        let mut cache = Cache::default();
        cache
            .ingest(
                &cast("unknown", "unknown", "Unknown", invalid, 120)
                    .to_vec()
                    .unwrap(),
                0,
            )
            .unwrap();
        assert_eq!(
            cache.candidates(0)[0].advertised_outputs,
            AdvertisedOutputs::default()
        );
    }
}

#[test]
fn duplicate_names_remain_separate_addresses_merge_and_identity_survives_rename() {
    let mut cache = Cache::default();
    let mut first = cast("first", "id-a", "TV", "5", 120);
    let host = Name::from_ascii("first.local.").unwrap();
    first.add_additional(Record::from_rdata(
        host,
        120,
        RData::A(A(Ipv4Addr::new(192, 168, 1, 21))),
    ));
    cache.ingest(&first.to_vec().unwrap(), 0).unwrap();
    cache
        .ingest(&cast("second", "id-b", "TV", "5", 120).to_vec().unwrap(), 0)
        .unwrap();
    let devices = cache.candidates(0);
    assert_eq!(devices.len(), 2);
    assert_ne!(devices[0].key, devices[1].key);
    assert_eq!(devices[0].endpoints.len(), 2);
    let key = devices[0].key.clone();
    let mut renamed = cast("first", "id-a", "Renamed TV", "5", 120);
    for record in &mut renamed.additionals {
        record.mdns_cache_flush = true;
    }
    cache.ingest(&renamed.to_vec().unwrap(), 1).unwrap();
    assert_eq!(
        cache
            .candidates(1)
            .iter()
            .find(|device| device.key == key)
            .unwrap()
            .display_name,
        "Renamed TV"
    );
}

#[test]
fn split_responses_are_resolved_and_goodbye_removes_target() {
    let message = cast("split", "split-id", "AzubaTV", "5", 2);
    let mut ptr = message.clone();
    ptr.additionals.clear();
    let mut cache = Cache::default();
    cache.ingest(&ptr.to_vec().unwrap(), 0).unwrap();
    assert!(cache.candidates(0).is_empty());
    assert_eq!(cache.resolution_queries().len(), 2);
    for record in message.additionals {
        let mut response = Message::response(0, OpCode::Query);
        response.add_answer(record);
        cache.ingest(&response.to_vec().unwrap(), 0).unwrap();
    }
    assert_eq!(cache.candidates(0).len(), 1);
    assert!(cache.resolution_queries().is_empty());
    for record in &mut ptr.answers {
        record.ttl = 0;
    }
    cache.ingest(&ptr.to_vec().unwrap(), 10).unwrap();
    assert!(cache.candidates(10).is_empty());
    assert!(cache.candidates(2000).is_empty());
}

#[test]
fn hostile_txt_is_bounded_sanitized_and_ambiguous_keys_rejected() {
    let mut message = cast("hostile", "hostile", "TV\n\u{202e}name", "5", 120);
    let mut cache = Cache::default();
    cache.ingest(&message.to_vec().unwrap(), 0).unwrap();
    assert_eq!(cache.candidates(0)[0].display_name, "TVname");
    let txt = message
        .additionals
        .iter_mut()
        .find(|record| matches!(record.data, RData::TXT(_)))
        .unwrap();
    txt.mdns_cache_flush = true;
    txt.data = RData::TXT(TXT::new(vec![
        "fn=one".into(),
        "fn=two".into(),
        "ca=5".into(),
    ]));
    cache.ingest(&message.to_vec().unwrap(), 1).unwrap();
    assert!(cache.candidates(1).is_empty());
}

#[test]
fn same_device_advertised_by_multiple_protocols_is_not_falsely_trusted_or_merged() {
    let mut cache = Cache::default();
    cache
        .ingest(&cast("room", "room", "Room", "5", 120).to_vec().unwrap(), 0)
        .unwrap();
    cache
        .ingest(
            &advertisement(
                "Room",
                SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 20), 45831),
                true,
                120,
            )
            .unwrap(),
            0,
        )
        .unwrap();
    let devices = cache.candidates(0);
    assert_eq!(devices.len(), 2);
    assert_ne!(devices[0].key, devices[1].key);
}

#[tokio::test]
#[ignore = "requires at least one receiver on the LAN and multicast access"]
async fn test_native_lan_discovery_finds_receivers() -> Result<()> {
    let devices = browse(Duration::from_secs(5)).await?;
    // Generic: any receiver qualifies. Optionally target one unit by name.
    let wanted = std::env::var("JANUS_TEST_RECEIVER_NAME").ok();
    anyhow::ensure!(
        devices.iter().any(|device| !device.endpoints.is_empty()
            && wanted
                .as_ref()
                .is_none_or(|name| &device.display_name == name)),
        "no matching receiver discovered"
    );
    Ok(())
}

#[tokio::test]
async fn browser_can_pause_without_opening_sockets_and_drop_closes_subscription() {
    let browser = Browser::start(&tokio::runtime::Handle::current());
    browser.set_enabled(false);
    let mut updates = browser.subscribe();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            updates.changed().await.unwrap();
            if updates.borrow_and_update().phase == BrowsePhase::Idle {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(updates.borrow().devices.is_empty());
    browser.retry();
    drop(browser);
    tokio::time::timeout(Duration::from_secs(1), async {
        while updates.changed().await.is_ok() {}
    })
    .await
    .unwrap();
}

#[test]
fn cast_requires_identity_hint_and_display_name_but_does_not_trust_them() {
    for missing in ["id=", "fn="] {
        let mut response = cast("receiver", "id", "TV", "5", 120);
        for record in &mut response.additionals {
            if let RData::TXT(txt) = &mut record.data {
                txt.txt_data = txt
                    .txt_data
                    .iter()
                    .filter(|entry| !entry.starts_with(missing.as_bytes()))
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_boxed_slice();
            }
        }
        let mut cache = Cache::default();
        cache.ingest(&response.to_vec().unwrap(), 0).unwrap();
        assert!(cache.candidates(0).is_empty());
    }
}

#[test]
fn cache_flush_preserves_recent_split_addresses_then_expires_replaced_routes() {
    let mut cache = Cache::default();
    cache
        .ingest(
            &cast("split-ips", "id", "TV", "5", 120).to_vec().unwrap(),
            0,
        )
        .unwrap();
    let host = Name::from_ascii("split-ips.local.").unwrap();
    let mut new_address =
        Record::from_rdata(host, 120, RData::A(A(Ipv4Addr::new(192, 168, 1, 21))));
    new_address.mdns_cache_flush = true;
    let mut response = Message::response(0, OpCode::Query);
    response.add_answer(new_address.clone());
    cache.ingest(&response.to_vec().unwrap(), 100).unwrap();
    assert_eq!(cache.candidates(100)[0].endpoints.len(), 2);
    cache.ingest(&response.to_vec().unwrap(), 2000).unwrap();
    assert_eq!(cache.candidates(2999)[0].endpoints.len(), 2);
    let devices = cache.candidates(3000);
    assert_eq!(devices[0].endpoints.len(), 1);
    assert_eq!(devices[0].endpoints[0].ip(), Ipv4Addr::new(192, 168, 1, 21));
}

#[test]
fn speaker_only_name_cannot_turn_an_audio_receiver_into_a_screen() {
    let mut cache = Cache::default();
    // Even the reference hardware name grants no capability by itself.
    cache
        .ingest(
            &cast("speaker", "speaker-id", "AzubaTV", "4", 120)
                .to_vec()
                .unwrap(),
            0,
        )
        .unwrap();
    let device = cache.candidates(0).remove(0);
    assert!(!device.advertised_outputs.may_support(OutputKind::Video));
    assert!(device.advertised_outputs.may_support(OutputKind::Audio));
}

#[test]
fn long_and_conflicting_metadata_never_expand_the_candidate_budget() {
    let mut response = cast("bounded", "id", "TV", "5", 120);
    for record in &mut response.additionals {
        if let RData::TXT(txt) = &mut record.data {
            *txt = TXT::new((0..33).map(|_| "fn=TV".to_owned()).collect());
        }
    }
    let mut cache = Cache::default();
    cache.ingest(&response.to_vec().unwrap(), 0).unwrap();
    assert!(cache.candidates(0).is_empty());
}

fn airplay(label: &str, features: &str) -> Message {
    let service = Name::from_ascii(AIRPLAY_SERVICE).unwrap();
    let instance = Name::from_ascii(format!("{label}.{AIRPLAY_SERVICE}")).unwrap();
    let host = Name::from_ascii(format!("{label}.local.")).unwrap();
    let mut message = Message::response(0, OpCode::Query);
    message.add_answer(Record::from_rdata(
        service,
        120,
        RData::PTR(PTR(instance.clone())),
    ));
    message.add_additional(Record::from_rdata(
        instance.clone(),
        120,
        RData::SRV(SRV::new(0, 0, 7000, host.clone())),
    ));
    message.add_additional(Record::from_rdata(
        instance,
        120,
        RData::TXT(TXT::new(vec![
            "deviceid=66:8F:F7:43:D1:29".into(),
            format!("features={features}"),
            "model=AppleTV3,2".into(),
            "srcvers=220.68".into(),
        ])),
    ));
    message.add_additional(Record::from_rdata(
        host,
        120,
        RData::A(A(Ipv4Addr::new(192, 168, 1, 30))),
    ));
    message
}

#[test]
fn test_airplay_receivers_are_discovered_with_outputs_from_features() {
    let query = packet(&query().unwrap()).unwrap();
    assert!(
        query
            .queries
            .iter()
            .any(|q| *q.name() == Name::from_ascii(AIRPLAY_SERVICE).unwrap())
    );

    let mut cache = Cache::default();
    // Two-word features with screen (bit 7), video and audio bits set.
    cache
        .ingest(&airplay("Board", "0x527FFFF7,0x1E").to_vec().unwrap(), 0)
        .unwrap();
    // Audio-only receiver (bit 9 only).
    cache
        .ingest(&airplay("Speaker", "0x200").to_vec().unwrap(), 0)
        .unwrap();
    // Unparseable features: not a candidate.
    cache
        .ingest(&airplay("Broken", "zzz").to_vec().unwrap(), 0)
        .unwrap();
    let candidates = cache.candidates(1);
    assert_eq!(candidates.len(), 2, "{candidates:?}");
    let board = candidates
        .iter()
        .find(|c| c.display_name == "Board")
        .unwrap();
    assert_eq!(board.protocol, ReceiverProtocol::AirPlay);
    assert_eq!(board.model.as_deref(), Some("AppleTV3,2"));
    assert_eq!(board.advertised_id.as_deref(), Some("66:8F:F7:43:D1:29"));
    assert_eq!(board.advertised_outputs.video, Some(true));
    assert_eq!(
        board.endpoints,
        vec![std::net::SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::new(192, 168, 1, 30),
            7000
        ))]
    );
    let speaker = candidates
        .iter()
        .find(|c| c.display_name == "Speaker")
        .unwrap();
    assert_eq!(
        (
            speaker.advertised_outputs.video,
            speaker.advertised_outputs.audio
        ),
        (Some(false), Some(true))
    );
    assert_eq!(
        serde_json::to_value(ReceiverProtocol::AirPlay).unwrap(),
        "airplay"
    );
}
