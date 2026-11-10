use janus_airplay::Profile;
use janus_cli::acceptance::{
    Outcome, airplay_authentication, airplay_teardown, hisense_audio_transmission,
};

#[test]
fn authentication_evidence_matches_started_transport() {
    let Outcome::Pending(hisense) = airplay_authentication(Some(Profile::Hisense)) else {
        panic!("unencrypted EShare must not pass authentication");
    };
    assert!(hisense.contains("unencrypted"));
    assert!(hisense.contains("does not authenticate receiver identity"));
    assert!(matches!(airplay_authentication(None), Outcome::Pending(_)));
    let Outcome::Pass(paired) = airplay_authentication(Some(Profile::Paired)) else {
        panic!("paired transport verifies its stored pairing");
    };
    assert!(paired.contains("HomeKit pairing verified"));
    assert!(!paired.contains("FairPlay"));
    for profile in [Profile::Rtsp, Profile::LegacyHttp] {
        let Outcome::Pass(legacy) = airplay_authentication(Some(profile)) else {
            panic!("legacy transport completes key possession and FairPlay");
        };
        assert!(legacy.contains("FairPlay exchange completed"));
    }
}

#[test]
fn eshare_stop_does_not_claim_a_protocol_acknowledgement() {
    for confirmed in [None, Some(false), Some(true)] {
        let Outcome::Pending(detail) = airplay_teardown(Some(Profile::Hisense), confirmed) else {
            panic!("EShare local shutdown requires physical confirmation");
        };
        assert!(detail.contains("no RTSP teardown acknowledgement"));
        assert!(detail.contains("confirm display stopped"));
    }
    for profile in [Profile::Rtsp, Profile::Paired] {
        assert!(matches!(
            airplay_teardown(Some(profile), Some(true)),
            Outcome::Pass(_)
        ));
        for confirmed in [None, Some(false)] {
            assert!(matches!(
                airplay_teardown(Some(profile), confirmed),
                Outcome::Fail(_)
            ));
        }
    }
    assert!(matches!(
        airplay_teardown(Some(Profile::LegacyHttp), None),
        Outcome::Pending(_)
    ));
}

#[test]
fn eshare_audio_requires_transmission_and_physical_sound_confirmation() {
    assert!(matches!(hisense_audio_transmission(0), Outcome::Fail(_)));
    for packets in [1, 256, u64::MAX] {
        let Outcome::Pending(detail) = hisense_audio_transmission(packets) else {
            panic!("sent audio packets must never count as a playback acknowledgement");
        };
        assert!(detail.contains(&format!("{packets} audio packets transmitted")));
        assert!(detail.contains("no audio playback ACK"));
        assert!(detail.contains("confirm sound physically"));
    }
}
