//! H.264 bitstream structure, forced key frames, decode round trip, test
//! pattern motion, and capture-format conversion/scaling.
use janus_encode::{
    h264::{Error, H264Encoder, VideoConfig},
    pattern,
    scale::{self, PixelFormat},
};
use openh264::{OpenH264API, decoder::Decoder, formats::YUVSource};

const CONFIG: VideoConfig = VideoConfig {
    width: 640,
    height: 360,
    frame_rate: 30,
    bit_rate: 1_000_000,
};

/// NAL unit types in an Annex-B access unit.
fn nal_types(data: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0
            && data[i + 1] == 0
            && (data[i + 2] == 1 || (data[i + 2] == 0 && data[i + 3] == 1))
        {
            let start = if data[i + 2] == 1 { i + 3 } else { i + 4 };
            types.push(data[start] & 0x1f);
            i = start;
        } else {
            i += 1;
        }
    }
    types
}

#[test]
fn test_first_frame_is_idr_with_parameter_sets_then_predicted_frames() {
    let mut encoder = H264Encoder::new(CONFIG).unwrap();
    let mut frame = Vec::new();
    let mut outputs = Vec::new();
    for n in 0..10 {
        pattern::draw(n, 30, CONFIG.width, CONFIG.height, &mut frame);
        if let Some(encoded) = encoder.encode(&frame, false).unwrap() {
            outputs.push(encoded);
        }
    }
    assert!(outputs.len() >= 8, "encoder should not skip most frames");
    let first = &outputs[0];
    assert!(first.key_frame);
    let types = nal_types(&first.data);
    assert!(
        types.contains(&7) && types.contains(&8) && types.contains(&5),
        "{types:?}"
    );
    assert!(outputs[1..].iter().all(|frame| !frame.key_frame));
    assert!(nal_types(&outputs[1].data).contains(&1));
}

#[test]
fn test_forced_key_frame_and_decode_round_trip() {
    let mut encoder = H264Encoder::new(CONFIG).unwrap();
    let mut decoder = Decoder::new().unwrap();
    let mut frame = Vec::new();
    let mut decoded = 0;
    for n in 0..6 {
        pattern::draw(n, 30, CONFIG.width, CONFIG.height, &mut frame);
        let encoded = encoder.encode(&frame, n == 4).unwrap().unwrap();
        assert_eq!(encoded.key_frame, n == 0 || n == 4, "frame {n}");
        if let Some(picture) = decoder.decode(&encoded.data).unwrap() {
            assert_eq!(picture.dimensions(), (640, 360));
            decoded += 1;
        }
    }
    assert!(decoded >= 5, "decoded {decoded}");
    let _ = OpenH264API::from_source();
}

#[test]
fn test_encoder_rejects_bad_config_and_frame_size() {
    for bad in [
        VideoConfig {
            width: 641,
            ..CONFIG
        },
        VideoConfig {
            width: 200,
            ..CONFIG
        },
        VideoConfig {
            frame_rate: 0,
            ..CONFIG
        },
        VideoConfig {
            bit_rate: 1000,
            ..CONFIG
        },
    ] {
        assert!(matches!(H264Encoder::new(bad), Err(Error::Config)));
    }
    let mut encoder = H264Encoder::new(CONFIG).unwrap();
    assert!(matches!(
        encoder.encode(&[0; 16], false),
        Err(Error::FrameSize)
    ));
}

#[test]
fn test_pattern_moves_between_frames() {
    let (mut a, mut b) = (Vec::new(), Vec::new());
    pattern::draw(0, 30, 320, 240, &mut a);
    pattern::draw(15, 30, 320, 240, &mut b);
    assert_eq!(a.len(), 320 * 240 * 4);
    assert_ne!(a, b);
}

#[test]
fn test_fit_keeps_aspect_and_even_sizes() {
    assert_eq!(scale::fit(2560, 1600, 1920, 1080), (1728, 1080));
    assert_eq!(scale::fit(1920, 1080, 1920, 1080), (1920, 1080));
    assert_eq!(scale::fit(1281, 721, 1920, 1080), (1280, 720));
    assert_eq!(scale::fit(0, 10, 1920, 1080), (0, 0));
}

#[test]
fn test_to_bgra_honours_format_and_stride_and_scales() {
    // 2×2 source, 12-byte stride (4 bytes of row padding), RGBx order.
    let source = [
        10, 20, 30, 0, 40, 50, 60, 0, 9, 9, 9, 9, //
        70, 80, 90, 0, 100, 110, 120, 0, 9, 9, 9, 9,
    ];
    let mut out = Vec::new();
    assert!(scale::to_bgra(
        &source,
        2,
        2,
        12,
        PixelFormat::Rgbx,
        (2, 2),
        &mut out
    ));
    assert_eq!(&out[..8], &[30, 20, 10, 255, 60, 50, 40, 255]);
    assert_eq!(&out[8..12], &[90, 80, 70, 255]);
    // Downscale to 1×1 averages the area: R 55, G 65, B 75.
    assert!(scale::to_bgra(
        &source,
        2,
        2,
        12,
        PixelFormat::Rgbx,
        (1, 1),
        &mut out
    ));
    assert_eq!(out, [75, 65, 55, 255]);
    // BGRx passes channels through.
    assert!(scale::to_bgra(
        &source,
        2,
        2,
        12,
        PixelFormat::Bgrx,
        (1, 1),
        &mut out
    ));
    assert_eq!(out, [55, 65, 75, 255]);
    // Short input and bad stride are rejected.
    assert!(!scale::to_bgra(
        &source[..10],
        2,
        2,
        12,
        PixelFormat::Rgbx,
        (2, 2),
        &mut out
    ));
    assert!(!scale::to_bgra(
        &source,
        2,
        2,
        4,
        PixelFormat::Rgbx,
        (2, 2),
        &mut out
    ));
}

#[test]
fn test_downscale_averages_coverage_instead_of_skipping_pixels() {
    // One row of 1-pixel black/white strokes, like thin text.
    let row = |values: &[u8]| -> Vec<u8> { values.iter().flat_map(|v| [*v, *v, *v, 0]).collect() };
    let stripes = row(&[0, 255, 0, 255]);
    let mut out = Vec::new();
    // 4 → 2 (exactly half): each output is one black and one white pixel.
    assert!(scale::to_bgra(
        &stripes,
        4,
        1,
        16,
        PixelFormat::Bgrx,
        (2, 1),
        &mut out
    ));
    assert_eq!(out, [128, 128, 128, 255, 128, 128, 128, 255]);
    // 4 → 3, the 2560→1920 ratio: coverage 1 + 1/3, 2/3 + 2/3, 1/3 + 1.
    assert!(scale::to_bgra(
        &stripes,
        4,
        1,
        16,
        PixelFormat::Bgrx,
        (3, 1),
        &mut out
    ));
    assert_eq!(
        out.chunks(4).map(|p| p[0]).collect::<Vec<_>>(),
        [64, 128, 191]
    );
    // A uniform area keeps its exact value at any ratio.
    let grey = row(&[200; 7]);
    assert!(scale::to_bgra(
        &grey,
        7,
        1,
        28,
        PixelFormat::Bgrx,
        (3, 1),
        &mut out
    ));
    assert!(
        out.chunks(4)
            .all(|p| p[..3] == [200, 200, 200] && p[3] == 255)
    );
}
