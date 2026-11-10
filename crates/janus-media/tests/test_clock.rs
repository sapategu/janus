use janus_media::clock::*;
fn probe(at: u64, slope: f64, forward: u64, back: u64) -> Probe {
    let remote = |t: u64| (t as f64 * slope + 50_000.0).round() as u64;
    Probe {
        receiver_send: at,
        sender_receive: remote(at + forward),
        sender_send: remote(at + forward + 100),
        receiver_receive: at + forward + 100 + back,
    }
}

#[test]
fn fits_positive_and_negative_200ppm_without_unbounded_history() {
    for slope in [0.9998, 1.0002] {
        let mut estimator = ClockEstimator::default();
        let mut last = None;
        for n in 0..3600 {
            last = Some(
                estimator
                    .push(probe(1_000_000 + n * 500_000, slope, 1000, 1000))
                    .unwrap(),
            );
        }
        let model = last.unwrap();
        assert!(model.drift_qualified);
        assert!((model.slope - slope).abs() < 0.000001);
        let at = 1_800_000_000;
        let mapped = model
            .receiver_time((at as f64 * slope + 50_000.0).round() as u64)
            .unwrap();
        assert!(mapped.abs_diff(at) <= 2);
        assert!(estimator.sample_count() <= MAX_SAMPLES);
    }
}

#[test]
fn asymmetric_path_does_not_claim_exact_offset() {
    let mut estimator = ClockEstimator::default();
    let model = estimator.push(probe(1_000_000, 1.0, 500, 3500)).unwrap();
    assert!(model.uncertainty_us >= 2000.0);
    assert!(!model.drift_qualified);
    assert!(estimator.push(probe(1_000_000, 1.0, 1000, 1000)).is_err());
    estimator.reset();
    assert_eq!(estimator.sample_count(), 0);
}

#[test]
fn rejects_reversed_stalled_and_overflowing_samples() {
    let mut estimator = ClockEstimator::default();
    assert!(
        estimator
            .push(Probe {
                receiver_send: 10,
                receiver_receive: 9,
                sender_receive: 10,
                sender_send: 11
            })
            .is_err()
    );
    assert!(estimator.push(probe(100, 1.0, 600_000, 1)).is_err());
}
