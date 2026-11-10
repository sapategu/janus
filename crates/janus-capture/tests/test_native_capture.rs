//! Opt-in native capture on a real desktop session.
//!
//! cargo test -p janus-capture --test test_native_capture -- --ignored --nocapture
//! The screen test shows the system picker: choose any screen or window.
#![cfg(target_os = "linux")]
use janus_capture::{SourceKind, audio::AudioStream, portal, video::VideoStream};
use std::time::{Duration, Instant};

#[test]
#[ignore = "needs a running PipeWire session"]
fn test_native_desktop_audio_delivers_48k_stereo_chunks() {
    let stream = AudioStream::start().expect("PipeWire audio monitor");
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut samples = 0;
    while Instant::now() < deadline && samples < 48_000 {
        match stream.try_recv() {
            Some(chunk) => {
                assert_eq!(chunk.samples.len() % 2, 0, "interleaved stereo");
                samples += chunk.samples.len() / 2;
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    stream.stop();
    // Silence still produces samples; ~1 s of audio should arrive in 3 s.
    assert!(samples >= 24_000, "only {samples} frames of audio in 3 s");
}

#[tokio::test]
#[ignore = "interactive: shows the system screen picker"]
async fn test_native_screen_capture_through_portal_delivers_frames() {
    let chosen = portal::select(SourceKind::Screen, None)
        .await
        .expect("portal selection");
    let stream = VideoStream::start(chosen.fd.try_clone().unwrap(), chosen.node_id)
        .expect("PipeWire stream");
    let mut frames = stream.frames();
    let frame = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(frame) = frames.borrow_and_update().clone() {
                return frame;
            }
            frames.changed().await.unwrap();
        }
    })
    .await
    .expect("a frame within 10 s");
    assert!(frame.width >= 320 && frame.height >= 240);
    assert!(frame.stride >= frame.width as usize * 4);
    assert!(frame.data.len() >= frame.stride * (frame.height as usize - 1));
    stream.stop();
    chosen.close().await;
}
