//! Output planning: size, frame rate and bitrate per encoder and receiver.
use janus_mirror::{Options, Plan, Resolution, paired_airplay_delay_ms, plan};

fn options(resolution: Resolution) -> Options {
    Options::default().with_resolution(resolution)
}

#[test]
fn test_auto_casts_the_full_source_size_at_the_highest_sustained_rate() {
    let qhd = (2560, 1440);
    // GPU: the screen's own 2K size, at the rate the encoder sustains there.
    let p = plan(qhd, &options(Resolution::Auto), 60, true);
    assert_eq!((p.width, p.height, p.frame_rate), (2560, 1440, 30));
    assert_eq!(p.bit_rate, (2560.0 * 1440.0 * 30.0 * 0.12) as u32);
    // A 1080p screen gets 60 fps on the GPU.
    let p = plan((1920, 1080), &options(Resolution::Auto), 60, true);
    assert_eq!((p.width, p.height, p.frame_rate), (1920, 1080, 60));
    // Never above the receiver's maximum: a 4K source on a 1080p receiver.
    let mut capped = options(Resolution::Auto);
    (capped.max_width, capped.max_height) = (1920, 1080);
    let p = plan((3840, 2160), &capped, 60, true);
    assert_eq!((p.width, p.height), (1920, 1080));
    // Software: full size at a rate it sustains.
    let p = plan(qhd, &options(Resolution::Auto), 60, false);
    assert_eq!((p.width, p.height, p.frame_rate), (2560, 1440, 15));
    // A smaller source is never upscaled.
    let p = plan((1366, 768), &options(Resolution::Auto), 60, true);
    assert_eq!((p.width, p.height), (1366, 768));
}

#[test]
fn test_manual_sizes_trade_frame_rate_for_resolution() {
    let uhd = (3840, 2160);
    let cases = [
        (Resolution::P720, true, (1280, 720, 60)),
        (Resolution::P1080, true, (1920, 1080, 60)),
        (Resolution::P1440, true, (2560, 1440, 30)),
        (Resolution::P2160, true, (3840, 2160, 20)),
        (Resolution::P720, false, (1280, 720, 60)),
        (Resolution::P1080, false, (1920, 1080, 30)),
        (Resolution::P2160, false, (3840, 2160, 15)),
    ];
    for (resolution, gpu, expected) in cases {
        let Plan {
            width,
            height,
            frame_rate,
            bit_rate,
        } = plan(uhd, &options(resolution), 60, gpu);
        assert_eq!(
            (width, height, frame_rate),
            expected,
            "{resolution:?} gpu={gpu}"
        );
        assert!((2_000_000..=30_000_000).contains(&bit_rate));
    }
    assert_eq!(Resolution::ALL.len(), 5);
}

#[test]
fn test_receiver_rate_and_explicit_settings_are_respected() {
    let source = (1920, 1080);
    // A receiver advertising 30 fps caps the plan.
    assert_eq!(
        plan(source, &options(Resolution::Auto), 30, true).frame_rate,
        30
    );
    // An explicit bitrate and a lower requested rate are kept.
    let mut explicit = options(Resolution::P1080);
    explicit.video_bit_rate = 5_000_000;
    explicit.frame_rate = 24;
    let p = plan(source, &explicit, 60, true);
    assert_eq!((p.frame_rate, p.bit_rate), (24, 5_000_000));
    // Sizes stay even and at least the Open Screen minimum.
    let p = plan((101, 77), &options(Resolution::Auto), 60, true);
    assert_eq!((p.width, p.height), (320, 240));
}

#[test]
fn test_resolution_labels_parse_and_cycle() {
    for r in Resolution::ALL {
        assert_eq!(Resolution::parse(r.label()), Some(r));
    }
    assert_eq!(Resolution::parse("4k"), Some(Resolution::P2160));
    assert_eq!(Resolution::default(), Resolution::Auto);
    assert_eq!(Resolution::P2160.next(), Resolution::Auto);
}

#[test]
fn test_paired_airplay_delay_grows_only_with_the_pipeline() {
    let at = |w, h| plan((w, h), &options(Resolution::Auto), 60, true);
    assert_eq!(paired_airplay_delay_ms(&at(1920, 1080), true), 150);
    assert_eq!(paired_airplay_delay_ms(&at(2560, 1440), true), 200);
    assert_eq!(paired_airplay_delay_ms(&at(3840, 2160), true), 300);
    assert_eq!(paired_airplay_delay_ms(&at(1920, 1080), false), 250);
}
