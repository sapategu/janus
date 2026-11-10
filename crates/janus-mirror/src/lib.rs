//! Cast pipeline: source → encoder → Cast Streaming, owned by one handle.
//!
//! [`start`] asks for the source (system picker for a screen or a window, or
//! a synthetic test pattern), negotiates the cast on an existing
//! [`Session`], then runs a video thread (latest frame → BGRA → H.264) and,
//! when audio is enabled, an audio thread (desktop monitor → Opus 10 ms).
//! Key-frame requests from the receiver are honoured. The pipeline stops on
//! [`Mirror::stop`], or by itself when the receiver ends mirroring or the
//! connection is lost.
use janus_cast::{
    offer::{Media, StreamOffer},
    session::{CastError, CastPhase, Session},
    streaming::{self, EncodedFrame, Streamer},
};
#[cfg(target_os = "linux")]
use janus_encode::opus::{FRAME_SAMPLES, OpusEncoder};
use janus_encode::{
    h264::VideoConfig,
    pattern,
    scale::{self, PixelFormat},
    video::{Backend, VideoEncoder},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::sync::watch;

#[cfg(target_os = "linux")]
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const VIDEO_INDEX: u32 = 1;
const AUDIO_INDEX: u32 = 0;

/// Output size presets for every protocol. A cast never exceeds the source
/// (no upscaling) or what the receiver advertises.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Resolution {
    /// The largest size both the shared screen and the receiver support.
    #[default]
    Auto,
    P720,
    P1080,
    P1440,
    P2160,
}
impl Resolution {
    pub const ALL: [Self; 5] = [
        Self::Auto,
        Self::P720,
        Self::P1080,
        Self::P1440,
        Self::P2160,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::P720 => "720p",
            Self::P1080 => "1080p",
            Self::P1440 => "2K",
            Self::P2160 => "4K",
        }
    }
    pub fn size(self) -> (u32, u32) {
        match self {
            Self::Auto | Self::P2160 => (3840, 2160),
            Self::P720 => (1280, 720),
            Self::P1080 => (1920, 1080),
            Self::P1440 => (2560, 1440),
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "720p" | "hd" => Some(Self::P720),
            "1080p" | "fhd" | "full-hd" => Some(Self::P1080),
            "1440p" | "2k" | "qhd" => Some(Self::P1440),
            "2160p" | "4k" | "uhd" => Some(Self::P2160),
            _ => None,
        }
    }
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|r| *r == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}
/// Paired AirPlay playout target: Doubletake 5e2f1cb latency.go's 75–100 ms
/// for hardware encoders plus the measured capture→encoded time at the planned
/// size (GPU 1080p ~35 ms, 1440p up to ~100 ms; software 30–50 ms at 1080p).
/// Just enough that frames are not late, so the picture neither cracks nor
/// lags more than needed.
pub fn paired_airplay_delay_ms(plan: &Plan, gpu: bool) -> u32 {
    let pixels = u64::from(plan.width) * u64::from(plan.height);
    match (gpu, pixels) {
        (true, p) if p <= 1920 * 1080 => 150,
        (true, p) if p <= 2560 * 1440 => 200,
        (true, _) => 300,
        (false, p) if p <= 1280 * 720 => 200,
        (false, p) if p <= 1920 * 1080 => 250,
        (false, _) => 350,
    }
}

/// Encoder settings for one cast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub width: u32,
    pub height: u32,
    pub frame_rate: u32,
    pub bit_rate: u32,
}
/// Picks the output: no upscaling beyond the source, no more than the
/// receiver supports, a frame rate the selected encoder sustains at that
/// size (measured on a Renoir laptop: GPU 1080p ≥ 60 fps; software 30–40 ms
/// per 1080p frame), and a bitrate proportional to pixels per second.
pub fn plan(source: (u32, u32), options: &Options, receiver_fps: u32, gpu: bool) -> Plan {
    // Auto: full quality, the source's own size up to the receiver's maximum
    // (user: 2560×1440 looked cleaner than a 1080p downscale). Smaller sizes
    // are a manual choice for smoother motion.
    let (width, height) = scale::fit(source.0, source.1, options.max_width, options.max_height);
    let (width, height) = (width.max(320), height.max(240));
    let pixels = u64::from(width) * u64::from(height);
    let ceiling = match (gpu, pixels) {
        (true, p) if p <= 1920 * 1080 => 60,
        (true, p) if p <= 2560 * 1440 => 30,
        (true, _) => 20,
        (false, p) if p <= 1280 * 720 => 60,
        (false, p) if p <= 1920 * 1080 => 30,
        (false, _) => 15,
    };
    let frame_rate = options
        .frame_rate
        .min(ceiling)
        .min(receiver_fps.max(5))
        .max(5);
    // Bits per pixel per frame for Constrained Baseline screen content.
    let density = 0.12;
    let bit_rate = if options.video_bit_rate > 0 {
        options.video_bit_rate
    } else {
        (pixels as f64 * f64::from(frame_rate) * density).clamp(2_000_000.0, 30_000_000.0) as u32
    };
    Plan {
        width,
        height,
        frame_rate,
        bit_rate,
    }
}

/// What to cast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A whole monitor, chosen in the system dialog.
    Screen,
    /// One application window, chosen in the system dialog.
    Window,
    /// Moving colour bars; no capture permission needed.
    TestPattern,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub source: Source,
    /// Include desktop audio (everything the computer plays, not only the
    /// chosen window).
    pub audio: bool,
    pub max_width: u32,
    pub max_height: u32,
    /// Highest frame rate wanted; the plan lowers it to what the encoder and
    /// receiver sustain.
    pub frame_rate: u32,
    /// Explicit video bitrate; 0 chooses one from size and frame rate.
    pub video_bit_rate: u32,
    pub audio_bit_rate: u32,
    /// Receiver playout delay; larger is smoother on busy Wi-Fi.
    pub target_delay_ms: u32,
    /// Explicit playout delay for every protocol; `None` uses each
    /// protocol's default.
    pub latency_ms: Option<u32>,
}

impl Options {
    /// Sets the size limit; the bitrate follows the planned size and rate.
    pub fn with_resolution(mut self, resolution: Resolution) -> Self {
        (self.max_width, self.max_height) = resolution.size();
        self
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            source: Source::Screen,
            audio: true,
            max_width: 3840,
            max_height: 2160,
            frame_rate: 60,
            video_bit_rate: 0,
            audio_bit_rate: 128_000,
            target_delay_ms: 400,
            latency_ms: None,
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MirrorError {
    #[error(transparent)]
    Capture(#[from] janus_capture::CaptureError),
    #[error("no picture arrived from the selected source")]
    NoFrames,
    #[error("couldn't start the encoder: {0}")]
    Encoder(String),
    #[error(transparent)]
    Cast(#[from] CastError),
    #[error(transparent)]
    AirPlay(#[from] janus_airplay::Error),
    #[error("this receiver protocol is not supported yet")]
    UnsupportedReceiver,
    #[error("receiver connection failed: {0}")]
    Connection(String),
}

/// Live pipeline counters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MirrorStats {
    pub airplay_profile: Option<janus_airplay::Profile>,
    pub transmitted_video_frames: u64,
    pub transmitted_audio_packets: u64,
    pub timing_requests: u64,
    pub timing_replies: u64,
    /// AirPlay timing datagrams that were not valid requests from the receiver.
    pub timing_ignored: u64,
    /// AirPlay receiver-side stream observations (fixed labels, no payloads).
    pub receiver_events: Vec<String>,
    /// Milliseconds from AirPlay stream start until the adapter ended sharing.
    pub stream_ended_after_ms: Option<u64>,
    pub width: u32,
    pub height: u32,
    pub frame_rate: u32,
    pub bit_rate: u32,
    /// The hardware (VA-API) encoder is in use.
    pub gpu_encoder: bool,
    pub video_frames: u64,
    pub key_frames: u64,
    pub video_bytes: u64,
    pub audio_packets: u64,
    pub dropped_frames: u64,
    /// AirPlay: frames sent after their requested display time.
    pub late_frames: u64,
    /// Running average of capture-to-encoded time, milliseconds.
    pub encode_ms: u32,
    pub receiver_checkpoint: i64,
    pub round_trip_ms: u32,
    /// Set when the pipeline stopped by itself (receiver ended, lost link).
    pub ended: Option<String>,
    /// Some only when the adapter observed the receiver's teardown response.
    pub receiver_stop_confirmed: Option<bool>,
}

pub struct Mirror {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    stats: watch::Receiver<MirrorStats>,
    #[cfg(target_os = "linux")]
    portal: Option<janus_capture::portal::PortalCapture>,
    transport: Transport,
    publish: Arc<Mutex<watch::Sender<MirrorStats>>>,
}

impl Mirror {
    pub fn stats(&self) -> watch::Receiver<MirrorStats> {
        self.stats.clone()
    }

    pub fn is_running(&self) -> bool {
        !self.stop.load(Ordering::Relaxed)
    }

    /// Stops media, then the receiver's mirroring app and the portal session.
    pub async fn stop(self, session: &Session) {
        self.shutdown().await;
        session.stop_cast();
    }

    async fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Transport::AirPlay(sender) = &self.transport {
            sender.stop();
        }
        for thread in self.threads.drain(..) {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
        if let Transport::AirPlay(sender) = &self.transport {
            sender.close().await;
            let current = sender.stats().borrow().clone();
            let confirmed = match current.profile {
                janus_airplay::Profile::Rtsp | janus_airplay::Profile::Paired => {
                    Some(current.teardown_confirmed)
                }
                janus_airplay::Profile::LegacyHttp | janus_airplay::Profile::Hisense => None,
            };
            self.publish.lock().expect("stats lock").send_modify(|s| {
                s.receiver_stop_confirmed = confirmed;
                s.transmitted_video_frames = current.video_frames;
                s.transmitted_audio_packets = current.audio_packets;
                s.late_frames = current.video_late;
                s.timing_requests = current.timing_requests;
                s.timing_replies = current.timing_replies;
                s.timing_ignored = current.timing_ignored;
                s.receiver_events = current.receiver_events.clone();
                s.stream_ended_after_ms = current.ended_after_ms;
            });
        }
        #[cfg(target_os = "linux")]
        if let Some(portal) = self.portal.take() {
            portal.close().await;
        }
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Transport::AirPlay(sender) = &self.transport {
            sender.stop();
        }
    }
}

/// A frame source the video thread polls.
enum Video {
    #[cfg(target_os = "linux")]
    Capture(janus_capture::video::VideoStream),
    Pattern {
        width: u32,
        height: u32,
    },
}

#[cfg(target_os = "linux")]
fn layout_format(layout: janus_capture::PixelLayout) -> PixelFormat {
    match layout {
        janus_capture::PixelLayout::Bgrx => PixelFormat::Bgrx,
        janus_capture::PixelLayout::Rgbx => PixelFormat::Rgbx,
        janus_capture::PixelLayout::Xrgb => PixelFormat::Xrgb,
        janus_capture::PixelLayout::Xbgr => PixelFormat::Xbgr,
    }
}

/// Starts casting `options.source` on a connected `session`.
pub async fn start(session: &Session, mut options: Options) -> Result<Mirror, MirrorError> {
    if let Some(latency) = options.latency_ms {
        options.target_delay_ms = latency;
    }
    let prepared = prepare(options.source).await?;
    let gpu = janus_encode::video::gpu_available();
    let Plan {
        width,
        height,
        frame_rate,
        bit_rate,
    } = plan(prepared.source_size, &options, 60, gpu);
    options.frame_rate = frame_rate;
    options.video_bit_rate = bit_rate;
    let encoder = VideoEncoder::new(VideoConfig {
        width,
        height,
        frame_rate,
        bit_rate,
    })
    .map_err(|e| MirrorError::Encoder(e.to_string()))?;

    let mut offers = vec![StreamOffer {
        index: VIDEO_INDEX,
        media: Media::Video {
            codec: "h264",
            max_frame_rate: options.frame_rate,
            max_bit_rate: options.video_bit_rate,
            width,
            height,
        },
        ssrc: streaming::random_ssrc(false),
        target_delay_ms: options.target_delay_ms,
        aes_key: streaming::random_bytes16(),
        aes_iv_mask: streaming::random_bytes16(),
    }];
    if options.audio {
        offers.insert(
            0,
            StreamOffer {
                index: AUDIO_INDEX,
                media: Media::Audio {
                    codec: "opus",
                    bit_rate: options.audio_bit_rate,
                    channels: 2,
                },
                ssrc: streaming::random_ssrc(true),
                target_delay_ms: options.target_delay_ms,
                aes_key: streaming::random_bytes16(),
                aes_iv_mask: streaming::random_bytes16(),
            },
        );
    }

    let streamer = Arc::new(session.cast(offers).await?);
    let mirror = match finish(prepared, encoder, Transport::Cast(streamer), options) {
        Ok(mirror) => mirror,
        Err(error) => {
            session.stop_cast();
            return Err(error);
        }
    };
    let stop = mirror.stop.clone();
    let publish = mirror.publish.clone();

    // End the pipeline when the receiver stops mirroring or the link drops.
    let mut session_updates = session.subscribe();
    let watchdog_stop = stop.clone();
    let watchdog_publish = publish.clone();
    tokio::spawn(async move {
        loop {
            if watchdog_stop.load(Ordering::Relaxed) {
                return;
            }
            let reason = {
                let snapshot = session_updates.borrow_and_update();
                match &snapshot.cast {
                    CastPhase::EndedByReceiver => Some("The TV ended screen sharing.".to_owned()),
                    CastPhase::Failed(error) => Some(error.to_string()),
                    CastPhase::Stopped => Some("Stopped.".to_owned()),
                    _ => None,
                }
            };
            if let Some(reason) = reason {
                watchdog_stop.store(true, Ordering::Relaxed);
                watchdog_publish
                    .lock()
                    .expect("stats lock")
                    .send_modify(|stats| stats.ended = Some(reason));
                return;
            }
            if tokio::select! {result=session_updates.changed()=>result.is_err(),_=tokio::time::sleep(Duration::from_millis(100))=>false}
            {
                watchdog_stop.store(true, Ordering::Relaxed);
                return;
            }
        }
    });

    Ok(mirror)
}

fn video_loop(
    video: Video,
    mut encoder: VideoEncoder,
    transport: &Transport,
    stop: &AtomicBool,
    publish: &Mutex<watch::Sender<MirrorStats>>,
    epoch: Instant,
) {
    let config = encoder.config();
    let interval = Duration::from_secs_f64(1.0 / f64::from(config.frame_rate));
    let mut stream_stats = match transport {
        Transport::Cast(s) => s.stream(VIDEO_INDEX).and_then(|index| s.stats(index)),
        Transport::AirPlay(_) => None,
    };
    let mut handled_key_requests = 0;
    // AirPlay receivers (EShare `idr_req`) ask for key frames on their own
    // channel; their media queue is TCP, so a full queue means "wait".
    let airplay = match transport {
        Transport::AirPlay(s) => Some(s),
        Transport::Cast(_) => None,
    };
    let mut airplay_stats = airplay.map(|s| s.stats());
    let mut handled_receiver_keys = 0;
    // Encoded units waiting for queue room, oldest first (bounded).
    let mut outbox: std::collections::VecDeque<(u64, janus_encode::h264::EncodedVideo)> =
        std::collections::VecDeque::new();
    // After the picture stops changing, keep encoding the last frame for
    // about half a second so the encoder refines it to full sharpness.
    #[cfg(target_os = "linux")]
    let refine_ticks = (config.frame_rate / 2).max(1);
    #[cfg(target_os = "linux")]
    let mut refine_left = 0_u32;
    // Network-following bitrate (GPU encoder): down when the link backs up,
    // back up to the plan when it clears; checked once a second.
    let planned_bit_rate = config.bit_rate;
    let mut bit_rate = planned_bit_rate;
    let mut window = Instant::now();
    let mut blocked_ticks = 0_u32;
    #[cfg(target_os = "linux")]
    let mut last_sequence = 0;
    let mut bgra = Vec::new();
    let mut tick = 0_u64;
    let mut next = Instant::now();
    #[cfg(target_os = "linux")]
    let frames = match &video {
        Video::Capture(capture) => Some(capture.frames()),
        Video::Pattern { .. } => None,
    };
    let mut force_key = true;
    // Only the captured-screen path resends a static picture.
    #[cfg(target_os = "linux")]
    let mut last_sent = Instant::now() - Duration::from_secs(1);
    // Capture time of each frame in the encoder, by sequence number.
    let mut in_flight: std::collections::VecDeque<(u64, Instant)> =
        std::collections::VecDeque::new();
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now < next {
            std::thread::sleep(next - now);
        }
        next += interval;
        tick += 1;

        let (checkpoint, round_trip, key_requests) = stream_stats
            .as_mut()
            .map(|watch| {
                let current = watch.borrow_and_update();
                (
                    current.checkpoint,
                    current.round_trip_ms,
                    current.key_frame_requests,
                )
            })
            .unwrap_or((-1, 0, 0));
        if key_requests > handled_key_requests {
            handled_key_requests = key_requests;
            force_key = true;
        }
        if let Some(stats) = airplay_stats.as_mut() {
            let requests = stats.borrow_and_update().key_frame_requests;
            if requests > handled_receiver_keys {
                handled_receiver_keys = requests;
                force_key = true;
            }
        }
        // Back-pressure: while earlier units still wait for the link, don't
        // encode another frame; the newest capture is taken once it drains.
        let blocked = !outbox.is_empty() || airplay.is_some_and(|s| !s.video_ready());
        blocked_ticks += u32::from(blocked);
        if window.elapsed() >= Duration::from_secs(1) {
            let next = if blocked_ticks * 10 > config.frame_rate {
                (bit_rate / 5 * 4).max(planned_bit_rate / 4).max(1_000_000)
            } else if blocked_ticks == 0 {
                (bit_rate / 10 * 11).min(planned_bit_rate)
            } else {
                bit_rate
            };
            if next != bit_rate && airplay.is_some() && encoder.set_bit_rate(next) {
                bit_rate = next;
            }
            blocked_ticks = 0;
            window = Instant::now();
        }

        #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
        let mut captured = Instant::now();
        // Fill `bgra` with the frame to encode, or skip this tick.
        let fresh = match &video {
            _ if blocked => false,
            #[cfg(target_os = "linux")]
            Video::Capture(_) => {
                let frame = frames.as_ref().and_then(|f| f.borrow().clone());
                match frame {
                    Some(frame) if frame.sequence != last_sequence || force_key => {
                        if frame.sequence != last_sequence {
                            captured = frame.captured;
                        }
                        last_sequence = frame.sequence;
                        refine_left = refine_ticks;
                        scale::to_bgra(
                            &frame.data,
                            frame.width,
                            frame.height,
                            frame.stride,
                            layout_format(frame.layout),
                            (config.width, config.height),
                            &mut bgra,
                        )
                    }
                    // Picture just stopped changing: refine it.
                    Some(_) if refine_left > 0 && !bgra.is_empty() => {
                        refine_left -= 1;
                        true
                    }
                    // Static screen: resend about once a second so the
                    // receiver keeps a current picture.
                    Some(_) => last_sent.elapsed() >= Duration::from_secs(1) && !bgra.is_empty(),
                    None => false,
                }
            }
            Video::Pattern { width, height } => {
                pattern::draw(tick, config.frame_rate, *width, *height, &mut bgra);
                if (*width, *height) != (config.width, config.height) {
                    let source = std::mem::take(&mut bgra);
                    scale::to_bgra(
                        &source,
                        *width,
                        *height,
                        *width as usize * 4,
                        PixelFormat::Bgrx,
                        (config.width, config.height),
                        &mut bgra,
                    )
                } else {
                    true
                }
            }
        };
        // Finished units from earlier frames go out on every tick.
        let mut units = encoder.poll().unwrap_or_default();
        if fresh {
            in_flight.push_back((encoder.next_sequence(), captured));
            if in_flight.len() > 16 {
                in_flight.pop_front();
            }
            match encoder.encode(&bgra, force_key) {
                Ok(more) => {
                    units.extend(more);
                    force_key = false;
                }
                Err(_) => force_key = true,
            }
        }
        outbox.extend(units);
        if outbox.len() > 8 {
            // The link stalled for too long: start over from a key frame.
            let dropped = outbox.len() as u64;
            outbox.clear();
            force_key = true;
            publish
                .lock()
                .expect("stats lock")
                .send_modify(|stats| stats.dropped_frames += dropped);
        }
        while let Some((sequence, encoded)) = outbox.pop_front() {
            if airplay.is_some_and(|s| !s.video_ready()) {
                outbox.push_front((sequence, encoded));
                break;
            }
            while in_flight.front().is_some_and(|(s, _)| *s < sequence) {
                in_flight.pop_front();
            }
            let captured = match in_flight.front() {
                Some((s, at)) if *s == sequence => *at,
                _ => Instant::now(),
            };
            let encode_ms = captured.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
            let rtp_timestamp = (epoch.elapsed().as_secs_f64() * 90_000.0) as u64 as u32;
            let key_frame = encoded.key_frame;
            let bytes = encoded.data.len() as u64;
            let sent = match transport {
                Transport::Cast(s) => s.stream(VIDEO_INDEX).ok_or(()).and_then(|index| {
                    s.send(
                        index,
                        EncodedFrame {
                            key_frame,
                            rtp_timestamp,
                            data: encoded.data,
                        },
                    )
                    .map_err(|_| ())
                }),
                Transport::AirPlay(s) => s
                    .send_video(encoded.data, key_frame, captured)
                    .map_err(|_| ()),
            };
            if sent.is_err() {
                // Queue full: the next frame must be a key frame.
                force_key = true;
            } else {
                #[cfg(target_os = "linux")]
                {
                    last_sent = Instant::now();
                }
            }
            publish.lock().expect("stats lock").send_modify(|stats| {
                if sent.is_ok() {
                    stats.video_frames += 1;
                    stats.video_bytes += bytes;
                    stats.key_frames += u64::from(key_frame);
                } else {
                    stats.dropped_frames += 1;
                }
                stats.receiver_checkpoint = checkpoint;
                stats.round_trip_ms = round_trip;
                stats.encode_ms = if stats.encode_ms == 0 {
                    encode_ms
                } else {
                    (stats.encode_ms * 7 + encode_ms) / 8
                };
            });
        }
    }
    // Ends screen capture before the loop returns; the test pattern holds nothing.
    #[cfg(target_os = "linux")]
    drop(video);
}

#[cfg(target_os = "linux")]
fn audio_loop(
    capture: janus_capture::audio::AudioStream,
    mut encoder: OpusEncoder,
    streamer: &Streamer,
    stream: usize,
    stop: &AtomicBool,
    publish: &Mutex<watch::Sender<MirrorStats>>,
    epoch: Instant,
) {
    let mut rtp: Option<u32> = None;
    while !stop.load(Ordering::Relaxed) {
        let Some(chunk) = capture.try_recv() else {
            std::thread::sleep(Duration::from_millis(3));
            continue;
        };
        let start = *rtp.get_or_insert_with(|| {
            (chunk
                .captured
                .saturating_duration_since(epoch)
                .as_secs_f64()
                * 48_000.0) as u64 as u32
        });
        let Ok(packets) = encoder.push(&chunk.samples) else {
            continue;
        };
        let mut timestamp = start;
        let mut sent = 0;
        for packet in packets {
            if streamer
                .send(
                    stream,
                    EncodedFrame {
                        key_frame: true,
                        rtp_timestamp: timestamp,
                        data: packet,
                    },
                )
                .is_ok()
            {
                sent += 1;
            }
            timestamp = timestamp.wrapping_add(FRAME_SAMPLES as u32);
        }
        rtp = Some(timestamp);
        if sent > 0 {
            publish
                .lock()
                .expect("stats lock")
                .send_modify(|stats| stats.audio_packets += sent);
        }
    }
    capture.stop();
}

struct Prepared {
    video: Video,
    source_size: (u32, u32),
    #[cfg(target_os = "linux")]
    portal: Option<janus_capture::portal::PortalCapture>,
}
async fn prepare(source: Source) -> Result<Prepared, MirrorError> {
    #[cfg(target_os = "linux")]
    let mut portal = None;
    let (video, source_size) = match source {
        Source::TestPattern => (
            Video::Pattern {
                width: 1280,
                height: 720,
            },
            (1280, 720),
        ),
        #[cfg(target_os = "linux")]
        Source::Screen | Source::Window => {
            let kind = if source == Source::Screen {
                janus_capture::SourceKind::Screen
            } else {
                janus_capture::SourceKind::Window
            };
            let chosen = janus_capture::portal::select(kind, None).await?;
            let stream = janus_capture::video::VideoStream::start(
                chosen
                    .fd
                    .try_clone()
                    .map_err(|e| janus_capture::CaptureError::PipeWire(e.to_string()))?,
                chosen.node_id,
            )?;
            let mut frames = stream.frames();
            let first = tokio::time::timeout(FIRST_FRAME_TIMEOUT, async {
                loop {
                    if let Some(frame) = frames.borrow_and_update().clone() {
                        return Some(frame);
                    }
                    if frames.changed().await.is_err() {
                        return None;
                    }
                }
            })
            .await
            .ok()
            .flatten()
            .ok_or(MirrorError::NoFrames)?;
            portal = Some(chosen);
            (Video::Capture(stream), (first.width, first.height))
        }
        #[cfg(not(target_os = "linux"))]
        Source::Screen | Source::Window => {
            return Err(janus_capture::CaptureError::Unsupported.into());
        }
    };

    Ok(Prepared {
        video,
        source_size,
        #[cfg(target_os = "linux")]
        portal,
    })
}

mod receiver;
pub use receiver::{Casting, start_receiver, supports_receiver};

#[derive(Clone)]
enum Transport {
    Cast(Arc<Streamer>),
    AirPlay(janus_airplay::Sender),
}

fn finish(
    prepared: Prepared,
    encoder: VideoEncoder,
    transport: Transport,
    options: Options,
) -> Result<Mirror, MirrorError> {
    let config = encoder.config();
    let (publish, stats) = watch::channel(MirrorStats {
        width: config.width,
        height: config.height,
        frame_rate: config.frame_rate,
        bit_rate: config.bit_rate,
        gpu_encoder: encoder.backend() == Backend::Gpu,
        receiver_checkpoint: -1,
        airplay_profile: match &transport {
            Transport::AirPlay(sender) => Some(sender.stats().borrow().profile),
            Transport::Cast(_) => None,
        },
        ..MirrorStats::default()
    });
    let publish = Arc::new(Mutex::new(publish));
    let mut mirror = Mirror {
        stop: Arc::new(AtomicBool::new(false)),
        threads: Vec::new(),
        stats,
        transport: transport.clone(),
        publish: publish.clone(),
        #[cfg(target_os = "linux")]
        portal: prepared.portal,
    };
    let epoch = Instant::now();
    let stop = mirror.stop.clone();
    let video_publish = publish.clone();
    let video_transport = transport.clone();
    mirror.threads.push(
        std::thread::Builder::new()
            .name("janus-video-encode".into())
            .spawn(move || {
                video_loop(
                    prepared.video,
                    encoder,
                    &video_transport,
                    &stop,
                    &video_publish,
                    epoch,
                )
            })
            .map_err(|e| MirrorError::Encoder(e.to_string()))?,
    );
    #[cfg(target_os = "linux")]
    if options.audio {
        let stop = mirror.stop.clone();
        let publish = publish.clone();
        match &transport {
            Transport::Cast(sender) => {
                let stream = sender
                    .stream(AUDIO_INDEX)
                    .ok_or_else(|| MirrorError::Encoder("The TV did not accept sound.".into()))?;
                let encoder = OpusEncoder::new(options.audio_bit_rate as i32)
                    .map_err(|e| MirrorError::Encoder(e.to_string()))?;
                let capture = janus_capture::audio::AudioStream::start()?;
                let sender = sender.clone();
                mirror.threads.push(
                    std::thread::Builder::new()
                        .name("janus-audio-encode".into())
                        .spawn(move || {
                            audio_loop(capture, encoder, &sender, stream, &stop, &publish, epoch)
                        })
                        .map_err(|e| MirrorError::Encoder(e.to_string()))?,
                );
            }
            Transport::AirPlay(sender) => {
                let hisense = sender.stats().borrow().profile == janus_airplay::Profile::Hisense;
                let capture = janus_capture::audio::AudioStream::start_at_rate(44_100)?;
                let sender = sender.clone();
                mirror.threads.push(
                    std::thread::Builder::new()
                        .name("janus-airplay-audio".into())
                        .spawn(move || {
                            if hisense {
                                match janus_encode::aac::AacEncoder::new() {
                                    Ok(encoder) => {
                                        hisense_audio(capture, encoder, &sender, &stop, &publish)
                                    }
                                    Err(error) => {
                                        stop.store(true, Ordering::Relaxed);
                                        sender.stop();
                                        publish
                                            .lock()
                                            .expect("stats lock")
                                            .send_modify(|s| s.ended = Some(error.to_string()));
                                    }
                                }
                            } else {
                                airplay_audio(capture, &sender, &stop, &publish);
                            }
                        })
                        .map_err(|e| MirrorError::Encoder(e.to_string()))?,
                );
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    if options.audio {
        return Err(janus_capture::CaptureError::Unsupported.into());
    }
    if let Transport::AirPlay(sender) = transport {
        let mut updates = sender.stats();
        let stop = mirror.stop.clone();
        tokio::spawn(async move {
            loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let current = updates.borrow_and_update().clone();
                publish.lock().expect("stats lock").send_modify(|s| {
                    s.transmitted_video_frames = current.video_frames;
                    s.transmitted_audio_packets = current.audio_packets;
                    s.late_frames = current.video_late;
                    s.late_frames = current.video_late;
                    s.timing_requests = current.timing_requests;
                    s.timing_replies = current.timing_replies;
                    s.timing_ignored = current.timing_ignored;
                    s.receiver_events = current.receiver_events.clone();
                    s.stream_ended_after_ms = current.ended_after_ms;
                });
                if let Some(reason) = current.ended {
                    stop.store(true, Ordering::Relaxed);
                    publish
                        .lock()
                        .expect("stats lock")
                        .send_modify(|s| s.ended = Some(reason));
                    return;
                }
                if tokio::select! {r=updates.changed()=>r.is_err(),_=tokio::time::sleep(Duration::from_millis(100))=>false}
                {
                    stop.store(true, Ordering::Relaxed);
                    return;
                }
            }
        });
    }
    Ok(mirror)
}

async fn start_airplay(
    endpoints: &[std::net::SocketAddr],
    mut options: Options,
) -> Result<Mirror, MirrorError> {
    let capability = janus_airplay::probe(endpoints).await?;
    if !capability.screen {
        return Err(janus_airplay::Error::Unsupported.into());
    }
    if capability.pairing_required {
        // A PIN receiver Janus can pair with is a user step, not a failure.
        return Err(if capability.pin_pairing {
            janus_airplay::Error::NotPaired
        } else {
            janus_airplay::Error::PairingRequired
        }
        .into());
    }
    if options.audio && !capability.audio {
        return Err(janus_airplay::Error::AudioUnsupported.into());
    }
    #[cfg(target_os = "linux")]
    if options.audio
        && capability.profile == janus_airplay::Profile::Hisense
        && !janus_encode::aac::available()
    {
        return Err(MirrorError::Encoder(
            "The AAC-ELD audio encoder could not start.".into(),
        ));
    }
    options.max_width = options.max_width.min(capability.width);
    options.max_height = options.max_height.min(capability.height);
    let gpu = janus_encode::video::gpu_available();
    let prepared = prepare(options.source).await?;
    let planned = plan(prepared.source_size, &options, capability.max_fps, gpu);
    if let Some(latency) = options.latency_ms {
        options.target_delay_ms = latency;
    } else if capability.profile == janus_airplay::Profile::Paired {
        options.target_delay_ms = options
            .target_delay_ms
            .min(paired_airplay_delay_ms(&planned, gpu));
    }
    let Plan {
        width,
        height,
        frame_rate,
        bit_rate,
    } = planned;
    options.frame_rate = frame_rate;
    options.video_bit_rate = bit_rate;
    let encoder = VideoEncoder::new(VideoConfig {
        width,
        height,
        frame_rate,
        bit_rate,
    })
    .map_err(|e| MirrorError::Encoder(e.to_string()))?;
    let sender = janus_airplay::start(
        endpoints,
        options.audio,
        encoder.config().width,
        encoder.config().height,
        options.target_delay_ms,
    )
    .await?;
    let cleanup = sender.clone();
    match finish(prepared, encoder, Transport::AirPlay(sender), options) {
        Ok(m) => Ok(m),
        Err(e) => {
            cleanup.close().await;
            Err(e)
        }
    }
}

#[cfg(target_os = "linux")]
fn airplay_audio(
    capture: janus_capture::audio::AudioStream,
    sender: &janus_airplay::Sender,
    stop: &AtomicBool,
    publish: &Mutex<watch::Sender<MirrorStats>>,
) {
    let mut samples = std::collections::VecDeque::new();
    let mut captured = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let Some(chunk) = capture.try_recv() else {
            std::thread::sleep(Duration::from_millis(3));
            continue;
        };
        if samples.is_empty() {
            captured = chunk.captured;
        }
        if samples.len() + chunk.samples.len() > 44_100 / 2 {
            stop.store(true, Ordering::Relaxed);
            publish
                .lock()
                .expect("stats lock")
                .send_modify(|s| s.ended = Some("Sound capture fell behind.".into()));
            break;
        }
        samples.extend(chunk.samples.into_iter().map(|v| {
            if v.is_finite() {
                (v.clamp(-1., 1.) * 32767.).round() as i16
            } else {
                0
            }
        }));
        while samples.len() >= janus_airplay::AUDIO_SAMPLES * 2 {
            let packet = samples.drain(..janus_airplay::AUDIO_SAMPLES * 2).collect();
            match sender.send_audio(packet, captured) {
                Ok(()) => {}
                // A late packet is dropped like a late video frame.
                Err(janus_airplay::Error::QueueFull) => {
                    captured +=
                        Duration::from_secs_f64(janus_airplay::AUDIO_SAMPLES as f64 / 44100.);
                    continue;
                }
                // The engine has ended; its watchdog publishes the real reason.
                Err(janus_airplay::Error::Stopped) => break,
                Err(error) => {
                    stop.store(true, Ordering::Relaxed);
                    publish
                        .lock()
                        .expect("stats lock")
                        .send_modify(|s| s.ended = Some(error.to_string()));
                    break;
                }
            }
            captured += Duration::from_secs_f64(janus_airplay::AUDIO_SAMPLES as f64 / 44100.);
            publish
                .lock()
                .expect("stats lock")
                .send_modify(|s| s.audio_packets += 1);
        }
    }
    capture.stop();
}

#[cfg(target_os = "linux")]
fn hisense_audio(
    capture: janus_capture::audio::AudioStream,
    mut encoder: janus_encode::aac::AacEncoder,
    sender: &janus_airplay::Sender,
    stop: &AtomicBool,
    publish: &Mutex<watch::Sender<MirrorStats>>,
) {
    let mut origin = None;
    while !stop.load(Ordering::Relaxed) {
        let encoded = if let Some(chunk) = capture.try_recv() {
            origin.get_or_insert(chunk.captured);
            encoder.push(&chunk.samples)
        } else {
            std::thread::sleep(Duration::from_millis(3));
            encoder.poll()
        };
        let encoded = match encoded {
            Ok(packets) => packets,
            Err(error) => {
                stop.store(true, Ordering::Relaxed);
                publish
                    .lock()
                    .expect("stats lock")
                    .send_modify(|s| s.ended = Some(error.to_string()));
                break;
            }
        };
        for (position, data) in encoded {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let Some(origin) = origin else {
                break;
            };
            let captured = origin + Duration::from_secs_f64(position as f64 / 44_100.);
            match sender.send_aac(data, captured) {
                Ok(()) => publish
                    .lock()
                    .expect("stats lock")
                    .send_modify(|s| s.audio_packets += 1),
                Err(janus_airplay::Error::QueueFull) => continue,
                Err(janus_airplay::Error::Stopped) => break,
                Err(error) => {
                    stop.store(true, Ordering::Relaxed);
                    publish
                        .lock()
                        .expect("stats lock")
                        .send_modify(|s| s.ended = Some(error.to_string()));
                    break;
                }
            }
        }
    }
    capture.stop();
}
