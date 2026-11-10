//! Encoder selection. GPU throughput is an opt-in hardware test.
use janus_encode::{
    h264::VideoConfig,
    pattern,
    video::{Backend, VideoEncoder},
};
use std::time::Instant;

fn nal_types(unit: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..unit.len().saturating_sub(3) {
        if unit[i..i + 3] == [0, 0, 1] {
            out.push(unit[i + 3] & 31);
        }
    }
    out
}

/// (sequence, key frame, NAL unit types) per output unit.
type Units = Vec<(u64, bool, Vec<u8>)>;

fn run(width: u32, height: u32, frames: u64) -> (Backend, Units, f64) {
    let config = VideoConfig {
        width,
        height,
        frame_rate: 60,
        bit_rate: 12_000_000,
    };
    let mut encoder = VideoEncoder::new(config).unwrap();
    let mut bgra = Vec::new();
    let mut out = Vec::new();
    let start = Instant::now();
    for tick in 0..frames {
        pattern::draw(tick, 60, width, height, &mut bgra);
        for (sequence, unit) in encoder.encode(&bgra, tick == 0).unwrap() {
            out.push((sequence, unit.key_frame, nal_types(&unit.data)));
        }
    }
    // Collect what is still in flight (as the caller's next ticks would).
    for _ in 0..50 {
        for (sequence, unit) in encoder.poll().unwrap() {
            out.push((sequence, unit.key_frame, nal_types(&unit.data)));
        }
        if out.len() as u64 == frames {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    (encoder.backend(), out, start.elapsed().as_secs_f64())
}

#[test]
fn test_selected_encoder_starts_with_a_decodable_key_frame() {
    let (_, units, _) = run(640, 360, 20);
    assert!(!units.is_empty());
    let (sequence, key, nals) = &units[0];
    assert_eq!(*sequence, 0);
    assert!(*key);
    assert!(nals.contains(&7) && nals.contains(&8) && nals.contains(&5));
    // Sequence numbers rise and never exceed what was submitted.
    assert!(units.windows(2).all(|w| w[0].0 < w[1].0));
    assert!(units.iter().all(|u| u.0 < 20));
}

/// `cargo test -p janus-encode --test test_video -- --ignored --nocapture`
#[test]
#[ignore = "needs a VA-API H.264 encoder (GPU)"]
fn test_native_gpu_encoder_sustains_1080p60() {
    let (backend, units, seconds) = run(1920, 1080, 300);
    assert_eq!(backend, Backend::Gpu, "no GPU encoder on this system");
    let fps = units.len() as f64 / seconds;
    println!(
        "GPU 1080p: {} units in {seconds:.2} s = {fps:.0} fps, {} key",
        units.len(),
        units.iter().filter(|u| u.1).count()
    );
    assert_eq!(units.len(), 300);
    assert!(fps >= 60.0, "{fps:.0} fps");
}
