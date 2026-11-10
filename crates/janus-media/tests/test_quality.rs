use janus_media::quality::*;
fn feedback(bps: u64) -> Feedback {
    Feedback {
        useful_bps: bps,
        queue_age_us: 0,
        late_fraction: 0.0,
        audio_at_risk: false,
    }
}
#[test]
fn downshift_needs_two_windows_and_upshift_needs_five_seconds() {
    let mut a = Adaptation::default();
    assert_eq!(a.update(0, feedback(4_000_000)), Decision::Hold);
    assert!(matches!(
        a.update(200_000, feedback(4_000_000)),
        Decision::Change(_)
    ));
    for i in 1..=25 {
        assert_eq!(
            a.update(200_000 + i * 200_000, feedback(15_000_000)),
            Decision::Hold
        );
    }
    assert_eq!(
        a.update(5_400_000, feedback(15_000_000)),
        Decision::Change(LADDER[0])
    );
}
#[test]
fn audio_emergency_bypasses_hold_but_not_sample_rate() {
    let mut a = Adaptation::default();
    let mut f = feedback(1_000_000);
    f.audio_at_risk = true;
    for i in 0..3 {
        assert!(matches!(a.update(i * 200_000, f), Decision::Change(_)));
    }
    assert_eq!(a.update(400_000, f), Decision::Hold);
    assert_eq!(a.update(600_000, f), Decision::BelowMinimum);
}
