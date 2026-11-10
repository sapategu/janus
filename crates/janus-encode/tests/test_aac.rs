#![cfg(target_os = "linux")]

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSrc};
use janus_encode::aac::{AacEncoder, CHANNELS, Error, FRAME_SAMPLES, SAMPLE_RATE, available};
use std::time::{Duration, Instant};

fn tone(frame: usize) -> Vec<f32> {
    (0..FRAME_SAMPLES)
        .flat_map(|offset| {
            let time = (frame * FRAME_SAMPLES + offset) as f32 / SAMPLE_RATE as f32;
            [440., 880.].map(|hz| (time * hz * std::f32::consts::TAU).sin() * 0.4)
        })
        .collect()
}

#[test]
fn aac_units_decode_as_distinct_stereo_tones_without_adts() {
    gst::init().unwrap();
    assert!(available(), "the exact AAC-ELD codec must initialize");
    let mut encoder = AacEncoder::new().unwrap();
    let codec_data = encoder.audio_specific_config().to_vec();
    assert_eq!(codec_data, [0xf8, 0xe8, 0x50, 0x00]);
    assert_eq!(FRAME_SAMPLES, 480);
    let mut packets = Vec::new();
    for frame in 0..48 {
        packets.extend(encoder.push(&tone(frame)).unwrap());
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while packets.len() < 46 && Instant::now() < deadline {
        packets.extend(encoder.poll().unwrap());
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        packets.len() >= 44,
        "non-vacuous output: {} units",
        packets.len()
    );
    for (index, (sample_position, data)) in packets.iter().enumerate() {
        assert_eq!(*sample_position, (index * FRAME_SAMPLES) as u64);
        assert!(
            !data.is_empty() && data.len() < 1460,
            "bounded raw AAC access unit"
        );
        assert!(
            !(data[0] == 0xff && data.get(1).is_some_and(|b| b & 0xf0 == 0xf0)),
            "not ADTS"
        );
    }

    // Independent system FDK decoder consumes raw access units with the native
    // AAC-ELD / 480-sample / 44.1-kHz / stereo AudioSpecificConfig, not Janus's own decoder.
    let pipeline = gst::parse::launch(
        "appsrc name=source format=time \
         ! fdkaacdec ! audioconvert \
         ! audio/x-raw,format=F32LE,rate=44100,channels=2,layout=interleaved \
         ! appsink name=sink sync=false async=false",
    )
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let source = pipeline
        .by_name("source")
        .unwrap()
        .downcast::<AppSrc>()
        .unwrap();
    let sink = pipeline
        .by_name("sink")
        .unwrap()
        .downcast::<AppSink>()
        .unwrap();
    let caps = gst::Caps::builder("audio/mpeg")
        .field("mpegversion", 4i32)
        .field("stream-format", "raw")
        .field("rate", SAMPLE_RATE as i32)
        .field("channels", CHANNELS as i32)
        .field("codec_data", gst::Buffer::from_slice(codec_data))
        .build();
    source.set_caps(Some(&caps));
    pipeline.set_state(gst::State::Playing).unwrap();
    for (sample_position, data) in &packets {
        let mut buffer = gst::Buffer::from_mut_slice(data.clone());
        buffer
            .get_mut()
            .unwrap()
            .set_pts(gst::ClockTime::from_nseconds(
                sample_position * 1_000_000_000 / u64::from(SAMPLE_RATE),
            ));
        source.push_buffer(buffer).unwrap();
    }
    source.end_of_stream().unwrap();
    let mut pcm = Vec::new();
    let mut decoded = 0;
    while let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_seconds(2)) {
        let caps = sample.caps().unwrap();
        let structure = caps.structure(0).unwrap();
        assert_eq!(structure.get::<i32>("rate").unwrap(), SAMPLE_RATE as i32);
        assert_eq!(structure.get::<i32>("channels").unwrap(), CHANNELS as i32);
        let map = sample.buffer().unwrap().map_readable().unwrap();
        pcm.extend(
            map.chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap())),
        );
        decoded += 1;
        assert!(decoded < 100, "bounded decoder output");
    }
    let eos = sink.is_eos();
    let _ = pipeline.set_state(gst::State::Null);
    assert!(eos, "decoder completed rather than timing out");
    assert!(decoded >= 40 && pcm.len() >= 40 * FRAME_SAMPLES * CHANNELS);
    assert!(pcm.iter().all(|v| v.is_finite()));
    // Skip AAC priming. Spectral energy distinguishes the stereo channels and
    // verifies the expected rate, beyond merely accepting an encoded buffer.
    let stereo: Vec<_> = pcm.chunks_exact(2).skip(FRAME_SAMPLES * 2).collect();
    let amplitude = |channel: usize, hz: f64| {
        let mut re = 0.;
        let mut im = 0.;
        for (n, pair) in stereo.iter().enumerate() {
            let phase = n as f64 * hz * std::f64::consts::TAU / f64::from(SAMPLE_RATE);
            re += f64::from(pair[channel]) * phase.cos();
            im += f64::from(pair[channel]) * phase.sin();
        }
        re.hypot(im) / stereo.len() as f64
    };
    assert!(amplitude(0, 440.) > 0.1);
    assert!(amplitude(1, 880.) > 0.1);
    assert!(amplitude(0, 440.) > amplitude(0, 880.) * 10.);
    assert!(amplitude(1, 880.) > amplitude(1, 440.) * 10.);
}

#[test]
fn aac_input_is_bounded_and_nonfinite_samples_do_not_poison_codec() {
    let mut encoder = AacEncoder::new().unwrap();
    assert!(matches!(encoder.push(&[0.]), Err(Error::Input)));
    assert!(matches!(
        encoder.push(&vec![0.; FRAME_SAMPLES * CHANNELS * 17]),
        Err(Error::Input)
    ));
    assert!(encoder.push(&[]).unwrap().is_empty());
    let mut packets = Vec::new();
    let unusual = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 2., -2., 0.];
    for _ in 0..8 {
        let samples: Vec<_> = unusual
            .iter()
            .copied()
            .cycle()
            .take(FRAME_SAMPLES * CHANNELS)
            .collect();
        packets.extend(encoder.push(&samples).unwrap());
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while packets.len() < 6 && Instant::now() < deadline {
        packets.extend(encoder.poll().unwrap());
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(packets.len() >= 6);
}
