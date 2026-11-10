use janus_cli::fixture::*;

#[test]
fn flash_and_impulse_share_exact_sample_timeline_without_accumulated_rounding() {
    let mut pixels = vec![0; WIDTH * HEIGHT * 3 / 2];
    for frame in 0..3600 {
        picture(frame, &mut pixels);
        assert_eq!(pixels[WIDTH * 60 + 160] == 235, flash(frame));
        let sample = frame * SAMPLES_PER_FRAME;
        assert_eq!(audio_sample(sample) != 0, flash(frame));
        assert_eq!(audio_sample(sample + 48), 0);
    }
    let mut wav = Vec::new();
    audio(&mut wav, 120).unwrap();
    assert_eq!(wav.len(), 44 + 120 * SAMPLES_PER_FRAME as usize * 4);
    assert_eq!(&wav[..4], b"RIFF");
    let impulse = 44 + RATE as usize * 4;
    assert_eq!(&wav[impulse..impulse + 2], &12_000_i16.to_le_bytes());
}

#[test]
fn correlation_keeps_misses_and_signed_skew_and_rejects_unrelated_clocks() {
    let mut rows = vec![
        Observation {
            event_id: 0,
            capture_us: 100,
            source_audio_us: Some(95),
            display_us: Some(150),
            audio_us: Some(145),
        },
        Observation {
            event_id: 1,
            capture_us: 200,
            source_audio_us: Some(200),
            display_us: None,
            audio_us: Some(270),
        },
    ];
    let result = summarize(&rows, 2, 1).unwrap();
    assert_eq!(result.video_misses, 1);
    assert_eq!(result.paired_events, 1);
    assert_eq!(result.video_latency_p95_us, Some(50));
    assert_eq!(result.audio_latency_p95_us, Some(70));
    assert_eq!(result.signed_skew_min_us, Some(-5));
    assert_eq!(result.absolute_skew_p95_us, Some(5));
    assert!(summarize(&rows, 3, 1).is_err());
    rows[1].display_us = Some(199);
    assert!(summarize(&rows, 2, 1).is_err());
    rows[1].display_us = None;
    rows[1].event_id = 0;
    assert!(summarize(&rows, 2, 1).is_err());
    rows[1].event_id = 1;
    rows[0].source_audio_us = None;
    rows[1].source_audio_us = None;
    let result = summarize(&rows, 2, 1).unwrap();
    assert_eq!(result.audio_latency_p95_us, None);
    assert_eq!(result.audio_reference_misses, 2);
    assert_eq!(result.audio_misses, 0);
    assert_eq!(result.signed_skew_min_us, Some(-5));
}
