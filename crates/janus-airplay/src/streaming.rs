//! One owned AirPlay session: bounded media queues, fresh cryptography,
//! monotonic NTP mapping and local-first Stop. No media is sent by probe().
use crate::{
    Error, Profile,
    audio::{self, Audio},
    fairplay::random,
    hap,
    secure::Secure,
    session::{self, Setup},
    video::{MAX_FRAME, Video, timestamp},
    wire::Connection,
};
use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    sync::{mpsc, watch},
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub profile: Profile,
    pub video_frames: u64,
    pub video_bytes: u64,
    pub audio_packets: u64,
    pub ended: Option<String>,
    pub teardown_confirmed: bool,
    pub feedback_replies: u64,
    pub timing_requests: u64,
    pub timing_replies: u64,
    /// Datagrams on the timing socket that were not valid requests from the
    /// receiver's address (another address, wrong size or wrong mode).
    pub timing_ignored: u64,
    /// Bounded receiver-side observations on the stream connection.
    pub receiver_events: Vec<String>,
    /// Milliseconds from stream start until sharing ended.
    pub ended_after_ms: Option<u64>,
    /// Video frames whose last byte left after their requested display time
    /// (capture + playout delay). A receiver may drop such frames.
    pub video_late: u64,
    /// Key frames the receiver asked for (EShare `idr_req`); the encoder
    /// must answer each with an IDR.
    pub key_frame_requests: u64,
}
struct Frame {
    data: Vec<u8>,
    key: bool,
    captured: Instant,
}
enum SoundPayload {
    Pcm(Vec<i16>),
    Aac(Vec<u8>),
}
struct Sound {
    payload: SoundPayload,
    captured: Instant,
}
struct Inner {
    video: mpsc::Sender<Frame>,
    audio: mpsc::Sender<Sound>,
    audio_enabled: bool,
    stop: watch::Sender<bool>,
    stopped: AtomicBool,
    stats: watch::Receiver<Stats>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
#[derive(Clone)]
pub struct Sender {
    inner: Arc<Inner>,
}
impl std::fmt::Debug for Sender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AirPlaySender").finish_non_exhaustive()
    }
}
impl Sender {
    pub fn stats(&self) -> watch::Receiver<Stats> {
        self.inner.stats.clone()
    }
    /// Whether the bounded video queue can take another frame now. The
    /// stream is TCP: a caller that waits instead of dropping keeps the
    /// reference chain intact and needs no recovery key frame.
    pub fn video_ready(&self) -> bool {
        self.inner.video.capacity() > 0
    }
    pub fn send_video(&self, data: Vec<u8>, key: bool, captured: Instant) -> Result<(), Error> {
        if self.inner.stopped.load(Ordering::Acquire)
            || *self.inner.stop.borrow()
            || data.len() > MAX_FRAME
        {
            return Err(Error::Stopped);
        }
        self.inner
            .video
            .try_send(Frame {
                data,
                key,
                captured,
            })
            .map_err(|_| Error::QueueFull)
    }
    pub fn send_audio(&self, samples: Vec<i16>, captured: Instant) -> Result<(), Error> {
        if self.inner.stopped.load(Ordering::Acquire) || *self.inner.stop.borrow() {
            return Err(Error::Stopped);
        }
        if !self.inner.audio_enabled || self.inner.stats.borrow().profile == Profile::Hisense {
            return Err(Error::AudioUnsupported);
        }
        if samples.len() != audio::SAMPLES * 2 {
            return Err(Error::Protocol);
        }
        self.inner
            .audio
            .try_send(Sound {
                payload: SoundPayload::Pcm(samples),
                captured,
            })
            .map_err(|_| Error::QueueFull)
    }
    /// Queues one raw AAC-ELD access unit for the negotiated EShare audio stream.
    pub fn send_aac(&self, data: Vec<u8>, captured: Instant) -> Result<(), Error> {
        if self.inner.stopped.load(Ordering::Acquire) || *self.inner.stop.borrow() {
            return Err(Error::Stopped);
        }
        if !self.inner.audio_enabled || self.inner.stats.borrow().profile != Profile::Hisense {
            return Err(Error::AudioUnsupported);
        }
        if data.is_empty() || data.len() > 1460 {
            return Err(Error::Protocol);
        }
        self.inner
            .audio
            .try_send(Sound {
                payload: SoundPayload::Aac(data),
                captured,
            })
            .map_err(|_| Error::QueueFull)
    }
    /// Invalidates local sends synchronously, before waiting for the receiver.
    pub fn stop(&self) {
        self.inner.stopped.store(true, Ordering::Release);
        self.inner.stop.send_replace(true);
    }
    pub async fn close(&self) {
        self.stop();
        let mut stats = self.stats();
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if stats.borrow_and_update().ended.is_some() {
                    return;
                }
                if stats.changed().await.is_err() {
                    return;
                }
            }
        })
        .await;
    }
}
struct StartupStop(Option<watch::Sender<bool>>);
impl Drop for StartupStop {
    fn drop(&mut self) {
        if let Some(stop) = &self.0 {
            stop.send_replace(true);
        }
    }
}

/// Explicit Cast only. Socket establishment and all negotiation are bounded.
/// PIN/HAP-only profiles fail visibly; they never fall back to unencrypted video.
pub async fn start(
    endpoints: &[SocketAddr],
    audio: bool,
    width: u32,
    height: u32,
    delay_ms: u32,
) -> Result<Sender, Error> {
    if width < 320 || height < 240 || width > 3840 || height > 2160 || delay_ms > 2000 {
        return Err(Error::Protocol);
    }
    let ssrc = u32::from_be_bytes(random()?);
    let sequence = u16::from_be_bytes(random()?);
    let (mut control, info, endpoint) = session::connect(endpoints).await?;
    if width > info.capability.width || height > info.capability.height {
        return Err(Error::Unsupported);
    }
    if info.capability.profile == Profile::Hisense {
        return start_hisense(
            control,
            info,
            endpoint,
            MediaOptions {
                audio,
                width,
                height,
                delay_ms,
            },
        )
        .await;
    }
    if info.hap.is_some() {
        return start_paired(
            control,
            info,
            endpoint,
            MediaOptions {
                audio,
                width,
                height,
                delay_ms,
            },
        )
        .await;
    }
    if info.http.is_some() {
        return start_http(
            control,
            info,
            endpoint,
            MediaOptions {
                audio,
                width,
                height,
                delay_ms,
            },
        )
        .await;
    }
    let bind = if endpoint.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let timing = Arc::new(UdpSocket::bind(bind).await?);
    let audio_control = Arc::new(UdpSocket::bind(bind).await?);
    let audio_data = Arc::new(UdpSocket::bind(bind).await?);
    let (stop, stop_rx) = watch::channel(false);
    let mut guard = StartupStop(Some(stop.clone()));
    let origin = Instant::now();
    let timing_counts = Arc::new(TimingCounts::default());
    tokio::spawn(timing_loop(
        timing.clone(),
        endpoint.ip(),
        origin,
        stop_rx.clone(),
        timing_counts.clone(),
        Profile::Rtsp,
    ));
    let negotiated = session::negotiate(
        &mut control,
        &info,
        endpoint,
        Setup {
            audio,
            timing_port: timing.local_addr()?.port(),
            control_port: audio_control.local_addr()?.port(),
            delay_ms,
        },
    )
    .await
    .map_err(|error| match error {
        Error::RequestTimeout { stage } => Error::StartupTimeout {
            stage,
            timing_requests: timing_counts.requests.load(Ordering::Relaxed),
            timing_replies: timing_counts.replies.load(Ordering::Relaxed),
            timing_ignored: timing_counts.ignored.load(Ordering::Relaxed),
        },
        other => other,
    })?;
    let transport = async {
        let data = tokio::time::timeout(
            Duration::from_secs(3),
            TcpStream::connect(with_port(endpoint, negotiated.video_port)),
        )
        .await
        .map_err(|_| Error::RequestTimeout {
            stage: "video TCP connection",
        })??;
        data.set_nodelay(true)?;
        let event = match negotiated.event_port {
            Some(port) => Some(Events::new(
                tokio::time::timeout(
                    Duration::from_secs(3),
                    TcpStream::connect(with_port(endpoint, port)),
                )
                .await
                .map_err(|_| Error::RequestTimeout {
                    stage: "event TCP connection",
                })??,
                None,
            )?),
            None => None,
        };
        if !negotiated.skip_record {
            control
                .request(
                    "RECORD",
                    &negotiated.uri,
                    "",
                    &[],
                    &[("Range", "npt=0-"), ("RTP-Info", "seq=0;rtptime=0")],
                )
                .await?;
        }
        Ok::<_, Error>((data, event))
    }
    .await;
    let (data, event) = match transport {
        Ok(value) => value,
        Err(error) => {
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                control.request("TEARDOWN", &negotiated.uri, "", &[], &[]),
            )
            .await;
            return Err(error);
        }
    };
    if let Some(port) = negotiated.timing_port {
        tokio::spawn(timing_probes(
            timing,
            with_port(endpoint, port),
            origin,
            stop_rx.clone(),
        ));
    }
    let audio = match (negotiated.audio_port, negotiated.audio_control_port) {
        (Some(data_port), Some(control_port)) => Some(AudioTransport::Alac {
            encoder: Audio::new(negotiated.key, negotiated.iv, ssrc, sequence),
            data: audio_data,
            control: audio_control,
            data_peer: with_port(endpoint, data_port),
            control_peer: with_port(endpoint, control_port),
        }),
        _ => None,
    };
    let (read, data) = data.into_split();
    let ready = Ready {
        read: Some(read),
        lifecycle: Some(Lifecycle::Rtsp {
            control,
            uri: negotiated.uri,
        }),
        data,
        event,
        video: Video::new(negotiated.key, negotiated.video_id, width, height),
        audio,
        origin,
        timing_task: None,
        timing_counts,
    };
    guard.0 = None;
    Ok(launch(ready, Profile::Rtsp, stop, stop_rx, delay_ms))
}
const PAIRED_START: Duration = Duration::from_secs(15);
/// HomeKit-paired profile: pair-verify against the stored long-term key,
/// encrypted control, Doubletake's SETUP order, ChaCha20-Poly1305 video.
/// Never falls back to an unpaired or unencrypted path.
async fn start_paired(
    control: Connection<TcpStream>,
    info: session::Info,
    endpoint: SocketAddr,
    options: MediaOptions,
) -> Result<Sender, Error> {
    if info.password {
        return Err(Error::PairingRequired);
    }
    if options.audio && !info.capability.audio {
        return Err(Error::AudioUnsupported);
    }
    let kind = info.hap.ok_or(Error::Unsupported)?;
    let device = info.device.clone().ok_or(Error::NotPaired)?;
    let path = crate::pairings::default_store_path().ok_or(Error::NoPairingStore)?;
    let store = crate::pairings::Store::load(&path)?.ok_or(Error::NotPaired)?;
    let peer = store.receivers.get(&device).ok_or(Error::NotPaired)?;
    let mut control = control.map_stream(Secure::new);
    control.set_agent(crate::pairings::MODERN_AGENT);
    let shared = hap::verify(&mut control, kind, &store.identity, peer).await?;
    let keys = hap::control_keys(&shared);
    // Without FairPlay the reference publishes the control-key prefixes as
    // shk/shiv inside the encrypted channel; screen data uses its own HKDF key.
    let shk: [u8; 16] = keys.write[..16].try_into().expect("16 bytes");
    let shiv: [u8; 16] = keys.read[..16].try_into().expect("16 bytes");
    control.stream.enable(keys)?;
    let bind = if endpoint.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let timing = Arc::new(UdpSocket::bind(bind).await?);
    // The screen-audio descriptor always advertises a control port.
    let audio_control = Arc::new(UdpSocket::bind(bind).await?);
    let audio_data = Arc::new(UdpSocket::bind(bind).await?);
    let audio_key = random::<32>()?;
    let ssrc = u32::from_be_bytes(random()?);
    let sequence = u16::from_be_bytes(random()?);
    let (stop, stop_rx) = watch::channel(false);
    let mut guard = StartupStop(Some(stop.clone()));
    let origin = Instant::now();
    let timing_counts = Arc::new(TimingCounts::default());
    tokio::spawn(timing_loop(
        timing.clone(),
        endpoint.ip(),
        origin,
        stop_rx.clone(),
        timing_counts.clone(),
        Profile::Paired,
    ));
    let negotiated = session::negotiate_hap(
        &mut control,
        &info,
        endpoint,
        Setup {
            audio: options.audio,
            timing_port: timing.local_addr()?.port(),
            control_port: audio_control.local_addr()?.port(),
            delay_ms: options.delay_ms,
        },
        shk,
        shiv,
        audio_key,
    )
    .await
    .map_err(|error| match error {
        Error::RequestTimeout { stage } => Error::StartupTimeout {
            stage,
            timing_requests: timing_counts.requests.load(Ordering::Relaxed),
            timing_replies: timing_counts.replies.load(Ordering::Relaxed),
            timing_ignored: timing_counts.ignored.load(Ordering::Relaxed),
        },
        other => other,
    })?;
    let transport = async {
        let data = tokio::time::timeout(
            Duration::from_secs(3),
            TcpStream::connect(with_port(endpoint, negotiated.video_port)),
        )
        .await
        .map_err(|_| Error::RequestTimeout {
            stage: "video TCP connection",
        })??;
        data.set_nodelay(true)?;
        let event = match negotiated.event_port {
            Some(port) => Some(Events::new(
                tokio::time::timeout(
                    Duration::from_secs(3),
                    TcpStream::connect(with_port(endpoint, port)),
                )
                .await
                .map_err(|_| Error::RequestTimeout {
                    stage: "event TCP connection",
                })??,
                Some(hap::event_keys(&shared)),
            )?),
            None => None,
        };
        // Observed: with screen audio the receiver needed more than 5 s to
        // answer RECORD (the reference allows 30 s per request).
        if !negotiated.skip_record {
            control
                .request_within(
                    "RECORD",
                    &negotiated.uri,
                    "",
                    &[],
                    &[("Range", "npt=0-"), ("RTP-Info", "seq=0;rtptime=0")],
                    PAIRED_START,
                )
                .await?;
        }
        // The receiver's own volume is never changed: `volume: 0.000000` is
        // AirPlay full scale (0 dB) and set the TV to 100 % in user testing.
        Ok::<_, Error>((data, event))
    }
    .await;
    let (data, event) = match transport {
        Ok(value) => value,
        Err(error) => {
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                control.request("TEARDOWN", &negotiated.uri, "", &[], &[]),
            )
            .await;
            return Err(error);
        }
    };
    if let Some(port) = negotiated.timing_port {
        tokio::spawn(timing_probes(
            timing,
            with_port(endpoint, port),
            origin,
            stop_rx.clone(),
        ));
    }
    let (read, data) = data.into_split();
    let ready = Ready {
        read: Some(read),
        lifecycle: Some(Lifecycle::Paired {
            control: Box::new(control),
            uri: negotiated.uri,
        }),
        data,
        event,
        video: Video::paired(
            hap::video_key(&shared, negotiated.video_id),
            options.width,
            options.height,
        ),
        audio: match (negotiated.audio_port, negotiated.audio_control_port) {
            (Some(data_port), Some(control_port)) => Some(AudioTransport::Alac {
                encoder: Audio::paired(audio_key, ssrc, sequence),
                data: audio_data,
                control: audio_control.clone(),
                data_peer: with_port(endpoint, data_port),
                control_peer: with_port(endpoint, control_port),
            }),
            _ => None,
        },
        origin,
        timing_task: None,
        timing_counts,
    };
    drop(audio_control);
    guard.0 = None;
    Ok(launch(
        ready,
        Profile::Paired,
        stop,
        stop_rx,
        options.delay_ms,
    ))
}
async fn start_http(
    mut control: Connection<TcpStream>,
    info: session::Info,
    mut endpoint: SocketAddr,
    options: MediaOptions,
) -> Result<Sender, Error> {
    if info.capability.pairing_required {
        return Err(Error::PairingRequired);
    }
    if options.audio {
        return Err(Error::AudioUnsupported);
    }
    // Qualify the profile before authentication; failed raw proof is final.
    // The older HTTP service uses its own fresh FairPlay SAP and direct key/IV.
    endpoint.set_port(crate::http::MIRROR_PORT);
    let stream = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(endpoint))
        .await
        .map_err(|_| Error::RequestTimeout {
            stage: "HTTP mirroring connection",
        })??;
    stream.set_nodelay(true)?;
    let mut bind = control.stream.local_addr()?;
    bind.set_port(crate::http::TIMING_PORT);
    // Sole ownership until begin() succeeds: cancelling startup releases 7010.
    let timing = Arc::new(UdpSocket::bind(bind).await.map_err(|e| {
        Error::Io(format!(
            "couldn't bind legacy AirPlay timing port 7010: {e}"
        ))
    })?);
    let mut device = random::<6>()?;
    device[0] = (device[0] | 2) & 0xfe;
    let device = u64::from_be_bytes([
        0, 0, device[0], device[1], device[2], device[3], device[4], device[5],
    ]);
    let session_id = i32::from_be_bytes(random()?);
    let (data, key, origin) = crate::http::begin(
        &mut control,
        info.features,
        Connection::http(stream, endpoint),
        device,
        session_id,
        options.delay_ms,
    )
    .await?;
    let (stop, stop_rx) = watch::channel(false);
    let counts = Arc::new(TimingCounts::default());
    let task = tokio::spawn(timing_loop(
        timing,
        endpoint.ip(),
        origin,
        stop_rx.clone(),
        counts.clone(),
        Profile::LegacyHttp,
    ));
    let (read, data) = data.into_split();
    let ready = Ready {
        read: Some(read),
        lifecycle: Some(Lifecycle::Http {
            authentication: control,
        }),
        data,
        event: None,
        video: Video::http(key.raw, key.iv, options.width, options.height),
        audio: None,
        origin,
        timing_task: Some(task),
        timing_counts: counts,
    };
    Ok(launch(
        ready,
        Profile::LegacyHttp,
        stop,
        stop_rx,
        options.delay_ms,
    ))
}

async fn start_hisense(
    control: Connection<TcpStream>,
    info: session::Info,
    endpoint: SocketAddr,
    options: MediaOptions,
) -> Result<Sender, Error> {
    if info.capability.pairing_required {
        return Err(Error::PairingRequired);
    }
    let (stop, stop_rx) = watch::channel(false);
    let mut guard = StartupStop(Some(stop.clone()));
    let origin = Instant::now();
    crate::hisense::handshake_auxiliary(endpoint.ip(), stop_rx.clone()).await;
    let mut rtsp = crate::hisense::connect_rtsp(control, endpoint).await?;
    let negotiated = crate::hisense::negotiate(&mut rtsp).await?;
    let transport = async {
        let primary = with_port(endpoint, negotiated.video_port);
        let data =
            match tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(primary)).await {
                Ok(Ok(s)) => s,
                _ if negotiated.video_port == crate::hisense::DEFAULT_VIDEO_PORT => {
                    let fallback = with_port(endpoint, crate::hisense::FALLBACK_VIDEO_PORT);
                    tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(fallback))
                        .await
                        .map_err(|_| Error::RequestTimeout {
                            stage: "video TCP connection",
                        })??
                }
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => {
                    return Err(Error::RequestTimeout {
                        stage: "video TCP connection",
                    });
                }
            };
        data.set_nodelay(true)?;
        let audio = if options.audio {
            let port = rtsp.setup_audio().await?;
            let bind = if endpoint.is_ipv4() {
                "0.0.0.0:0"
            } else {
                "[::]:0"
            };
            Some(AudioTransport::Hisense {
                data: UdpSocket::bind(bind).await?,
                peer: with_port(endpoint, port),
                sequence: 0,
            })
        } else {
            None
        };
        let event = match negotiated.event_port {
            Some(port) => tokio::time::timeout(
                Duration::from_secs(2),
                TcpStream::connect(with_port(endpoint, port)),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|s| Events::new(s, None).ok()),
            None => None,
        };
        Ok::<_, Error>((data, event, audio))
    }
    .await;
    let (data, event, audio) = match transport {
        Ok(value) => value,
        Err(error) => {
            let _ = tokio::time::timeout(Duration::from_secs(2), rtsp.teardown()).await;
            return Err(error);
        }
    };
    let (read, data) = data.into_split();
    let ready = Ready {
        read: Some(read),
        lifecycle: Some(Lifecycle::Hisense { control: rtsp }),
        data,
        event,
        video: Video::hisense(options.width, options.height),
        audio,
        origin,
        timing_task: None,
        timing_counts: Arc::default(),
    };
    guard.0 = None;
    Ok(launch(
        ready,
        Profile::Hisense,
        stop,
        stop_rx,
        options.delay_ms,
    ))
}

struct MediaOptions {
    audio: bool,
    width: u32,
    height: u32,
    delay_ms: u32,
}
enum Lifecycle {
    Rtsp {
        control: Connection<TcpStream>,
        uri: String,
    },
    Paired {
        control: Box<Connection<Secure<TcpStream>>>,
        uri: String,
    },
    Http {
        authentication: Connection<TcpStream>,
    },
    Hisense {
        control: crate::hisense::RtspControl<TcpStream>,
    },
}
enum AudioTransport {
    Alac {
        encoder: Audio,
        data: Arc<UdpSocket>,
        control: Arc<UdpSocket>,
        data_peer: SocketAddr,
        control_peer: SocketAddr,
    },
    Hisense {
        data: UdpSocket,
        peer: SocketAddr,
        sequence: u16,
    },
}

struct Ready {
    lifecycle: Option<Lifecycle>,
    data: tokio::net::tcp::OwnedWriteHalf,
    read: Option<tokio::net::tcp::OwnedReadHalf>,
    event: Option<Events>,
    video: Video,
    audio: Option<AudioTransport>,
    origin: Instant,
    timing_task: Option<tokio::task::JoinHandle<()>>,
    timing_counts: Arc<TimingCounts>,
}
fn launch(
    transport: Ready,
    profile: Profile,
    stop: watch::Sender<bool>,
    stop_rx: watch::Receiver<bool>,
    delay_ms: u32,
) -> Sender {
    let (video_tx, video_rx) = mpsc::channel(2);
    let (audio_tx, audio_rx) = mpsc::channel(16);
    let (stats_tx, stats_rx) = watch::channel(Stats {
        profile,
        ..Stats::default()
    });
    let sender = Sender {
        inner: Arc::new(Inner {
            video: video_tx,
            audio: audio_tx,
            audio_enabled: transport.audio.is_some(),
            stop: stop.clone(),
            stopped: AtomicBool::new(false),
            stats: stats_rx,
        }),
    };
    let engine = Engine {
        transport,
        delay: Duration::from_millis(delay_ms.into()),
        stop: stop_rx,
        stats: stats_tx,
    };
    tokio::spawn(engine.run(video_rx, audio_rx, stop));
    sender
}
struct Engine {
    transport: Ready,
    delay: Duration,
    stop: watch::Receiver<bool>,
    stats: watch::Sender<Stats>,
}
impl Engine {
    async fn run(
        mut self,
        mut video: mpsc::Receiver<Frame>,
        mut audio: mpsc::Receiver<Sound>,
        stop: watch::Sender<bool>,
    ) {
        let lifecycle = self.transport.lifecycle.take().expect("owned lifecycle");
        let (control_task, authentication) = match lifecycle {
            Lifecycle::Rtsp { control, uri } => (
                Some(tokio::spawn(control_loop(
                    control,
                    uri,
                    stop.clone(),
                    self.stop.clone(),
                    self.stats.clone(),
                ))),
                None,
            ),
            Lifecycle::Paired { control, uri } => (
                Some(tokio::spawn(control_loop(
                    *control,
                    uri,
                    stop.clone(),
                    self.stop.clone(),
                    self.stats.clone(),
                ))),
                None,
            ),
            Lifecycle::Http { authentication } => (None, Some(authentication)),
            Lifecycle::Hisense { control } => (
                Some(tokio::spawn(crate::hisense::control_loop(
                    control,
                    stop.clone(),
                    self.stop.clone(),
                    self.stats.clone(),
                ))),
                None,
            ),
        };
        let http_task = if authentication.is_some() {
            let read = self.transport.read.take().expect("owned HTTP reader");
            Some(tokio::spawn(crate::http::monitor(
                read,
                stop.clone(),
                self.stop.clone(),
                self.transport.origin,
            )))
        } else {
            None
        };
        let result = self.stream(&mut video, &mut audio).await;
        let ended_after = self.transport.origin.elapsed().as_millis() as u64;
        stop.send_replace(true);
        let _ = self.transport.data.shutdown().await;
        drop(video);
        drop(audio);
        let (control_error, teardown) = match control_task {
            Some(task) => task.await.unwrap_or((Some(Error::Stopped), false)),
            None => (None, false),
        };
        drop(authentication);
        if let Some(task) = self.transport.timing_task.take() {
            task.abort();
            let _ = task.await;
        }
        let observed = match http_task {
            Some(task) => task.await.unwrap_or(crate::http::Observed {
                error: Some(Error::Stopped),
                events: Vec::new(),
            }),
            None => crate::http::Observed::default(),
        };
        let peer_error = control_error.or(observed.error);
        // A receiver's explicit rejection explains a later local write error.
        let result = match (result, peer_error) {
            (Err(Error::Io(_)), Some(e @ (Error::Status(_) | Error::PairingRequired))) => Err(e),
            (result, peer) => result.and(peer.map_or(Ok(()), Err)),
        };
        self.stats.send_modify(|s| {
            s.timing_requests = self
                .transport
                .timing_counts
                .requests
                .load(Ordering::Relaxed);
            s.timing_replies = self.transport.timing_counts.replies.load(Ordering::Relaxed);
            s.timing_ignored = self.transport.timing_counts.ignored.load(Ordering::Relaxed);
            s.receiver_events = observed.events;
            s.ended_after_ms = Some(ended_after);
            s.teardown_confirmed = teardown;
            s.ended = Some(
                result
                    .err()
                    .map_or_else(|| "Stopped.".into(), |e| e.to_string()),
            )
        });
    }
    async fn stream(
        &mut self,
        video: &mut mpsc::Receiver<Frame>,
        audio: &mut mpsc::Receiver<Sound>,
    ) -> Result<(), Error> {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Codec data opens the stream; heartbeats only follow the first frame.
        let mut first_video = false;
        let mut first_audio = true;
        let mut history = audio::History::new();
        let mut control_buf = [0; 128];
        let mut event_buf = [0; 1024];
        let mut data_buf = [0; 128];
        loop {
            if *self.stop.borrow() {
                return Ok(());
            }
            tokio::select! {
                _ = self.stop.changed() => return Ok(()),
                frame = video.recv() => {
                    let Some(frame)=frame else { return Ok(()) };
                    if *self.stop.borrow() { return Ok(()) }
                    let bytes=frame.data.len() as u64;
                    let time=frame.captured.saturating_duration_since(self.transport.origin)+self.delay;
                    for packet in self.transport.video.packets(&frame.data,frame.key,time)? {
                        write_packet(&mut self.transport.data,&packet,&mut self.stop).await?;
                    }
                    first_video=true;
                    let late = frame.captured.elapsed() > self.delay;
                    self.stats.send_modify(|s| {
                        s.video_frames+=1; s.video_bytes+=bytes; s.video_late+=u64::from(late);
                        s.timing_requests = self.transport.timing_counts.requests.load(Ordering::Relaxed);
                        s.timing_replies = self.transport.timing_counts.replies.load(Ordering::Relaxed);
                        s.timing_ignored = self.transport.timing_counts.ignored.load(Ordering::Relaxed);
                    });
                },
                sound = audio.recv(), if self.transport.audio.is_some() => {
                    let Some(sound)=sound else { return Ok(()) };
                    if *self.stop.borrow() { return Ok(()) }
                    let elapsed = sound.captured.saturating_duration_since(self.transport.origin);
                    match (self.transport.audio.as_mut().expect("audio branch"), sound.payload) {
                        (AudioTransport::Alac { encoder, data, control, data_peer, control_peer }, SoundPayload::Pcm(samples)) => {
                            let audio_time = (elapsed.as_secs_f64() * 44100.) as u64 as u32;
                            let ntp = timestamp(elapsed) + (2208988800u64 << 32);
                            let sync = audio::sync(audio_time, ntp, (self.delay.as_secs_f64() * 44100.) as u32, first_audio);
                            first_audio = false;
                            control.send_to(&sync, *control_peer).await?;
                            let packet = encoder.packet(&samples, audio_time)?;
                            data.send_to(&packet, *data_peer).await?;
                            history.remember(packet)?;
                        }
                        (AudioTransport::Hisense { data, peer, sequence }, SoundPayload::Aac(payload)) => {
                            let packet = crate::hisense::audio_packet(&payload, *sequence, elapsed.as_millis() as u32)?;
                            data.send_to(&packet, *peer).await?;
                            *sequence = sequence.wrapping_add(1);
                        }
                        _ => return Err(Error::Protocol),
                    }
                    self.stats.send_modify(|s| s.audio_packets += 1);
                },
                received = async {
                    match &self.transport.audio {
                        Some(AudioTransport::Alac { control, .. }) => control.recv_from(&mut control_buf).await,
                        _ => std::future::pending().await,
                    }
                } => {
                    let (n,from)=received?;
                    let Some(AudioTransport::Alac { control, control_peer, .. }) = &self.transport.audio else { return Err(Error::Protocol) };
                    if from.ip()!=control_peer.ip() { continue }
                    for response in history.responses(&control_buf[..n]) {
                        control.send_to(&response,from).await?;
                    }
                },
                _ = heartbeat.tick(), if first_video && self.transport.video.profile() != Profile::Hisense => {
                    let mut header=[0;128];header[4]=2;header[6]=0x1e;
                    write_packet(&mut self.transport.data,&header,&mut self.stop).await?;
                },
                read = async {
                    match &mut self.transport.event {
                        Some(e)=>e.stream.read(&mut event_buf).await,
                        None=>std::future::pending().await,
                    }
                } => {
                    let n=read?;
                    if n==0 { return Err(Error::ReceiverEnded) }
                    // The handler body is never cancelled, so a reply is
                    // written whole once its request has been consumed.
                    if let Some(e)=self.transport.event.as_mut() { e.receive(&event_buf[..n]).await?; }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                },
                read = async {
                    match &mut self.transport.read {
                        Some(read) => read.read(&mut data_buf).await,
                        None => std::future::pending().await,
                    }
                } => {
                    if read?==0 { return Err(Error::ReceiverEnded) }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                },
            }
        }
    }
}
/// The receiver's event connection. On the paired profile it is encrypted
/// with the Events keys and every receiver command gets an RTSP 200 (as the
/// reference sender does); older profiles keep ignore-only behaviour.
struct Events {
    stream: Secure<TcpStream>,
    reply: bool,
    pending: Vec<u8>,
}
impl Events {
    fn new(stream: TcpStream, keys: Option<hap::ChannelKeys>) -> Result<Self, Error> {
        let mut stream = Secure::new(stream);
        let reply = keys.is_some();
        if let Some(keys) = keys {
            stream.enable(keys)?;
        }
        Ok(Self {
            stream,
            reply,
            pending: Vec::new(),
        })
    }
    async fn receive(&mut self, data: &[u8]) -> Result<(), Error> {
        if !self.reply {
            return Ok(());
        }
        self.pending.extend_from_slice(data);
        while let Some((cseq, used)) = event_request(&self.pending)? {
            self.pending.drain(..used);
            let reply = match cseq {
                Some(cseq) => {
                    format!("RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nContent-Length: 0\r\n\r\n")
                }
                None => "RTSP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n".to_owned(),
            };
            tokio::time::timeout(Duration::from_secs(1), async {
                self.stream.write_all(reply.as_bytes()).await?;
                self.stream.flush().await
            })
            .await
            .map_err(|_| Error::Timeout)??;
        }
        Ok(())
    }
}
const EVENT_HEADER: usize = 16 * 1024;
const EVENT_BODY: usize = 64 * 1024;
/// One complete receiver request at the front of `buf`: (CSeq, bytes used).
/// Observed: a paired receiver's `POST /command` carries no CSeq.
pub(crate) fn event_request(buf: &[u8]) -> Result<Option<(Option<u32>, usize)>, Error> {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if buf.len() > EVENT_HEADER {
            Err(Error::Protocol)
        } else {
            Ok(None)
        };
    };
    if end > EVENT_HEADER {
        return Err(Error::Protocol);
    }
    let text = std::str::from_utf8(&buf[..end]).map_err(|_| Error::Protocol)?;
    let mut lines = text.split("\r\n");
    let first = lines.next().ok_or(Error::Protocol)?;
    if first.split_whitespace().count() != 3 || !first.ends_with(" RTSP/1.0") {
        return Err(Error::Protocol);
    }
    let mut cseq = None;
    let mut len = 0;
    for line in lines {
        let (k, v) = line.split_once(':').ok_or(Error::Protocol)?;
        match k.trim().to_ascii_lowercase().as_str() {
            "cseq" => cseq = Some(v.trim().parse().map_err(|_| Error::Protocol)?),
            "content-length" => {
                len = v.trim().parse::<usize>().map_err(|_| Error::Protocol)?;
                if len > EVENT_BODY {
                    return Err(Error::Protocol);
                }
            }
            _ => {}
        }
    }
    let total = end + 4 + len;
    if buf.len() < total {
        return Ok(None);
    }
    Ok(Some((cseq, total)))
}
#[derive(Default)]
struct TimingCounts {
    requests: AtomicU64,
    replies: AtomicU64,
    ignored: AtomicU64,
}

pub(crate) fn timing_reply(request: &[u8], time: u64) -> Option<[u8; 32]> {
    if request.len() < 32 || request[..2] != [0x80, 0xd2] {
        return None;
    }
    let mut reply = [0; 32];
    reply.copy_from_slice(&request[..32]);
    reply[0] = 0x80;
    reply[1] = 0xd3;
    reply[8..16].copy_from_slice(&request[24..32]);
    reply[16..24].copy_from_slice(&time.to_be_bytes());
    reply[24..32].copy_from_slice(&time.to_be_bytes());
    Some(reply)
}

async fn timing_loop(
    socket: Arc<UdpSocket>,
    peer: IpAddr,
    origin: Instant,
    mut stop: watch::Receiver<bool>,
    counts: Arc<TimingCounts>,
    profile: Profile,
) {
    let mut buf = [0; 128];
    loop {
        tokio::select! {
            biased;
            _ = stop.changed() => return,
            result = socket.recv_from(&mut buf) => {
                let Ok((n,from))=result else { return };
                let reply = if from.ip()==peer {
                    let time=timestamp(origin.elapsed());
                    match profile {
                        Profile::Rtsp | Profile::Paired | Profile::Hisense => timing_reply(&buf[..n], time+(2208988800u64<<32)).map(Vec::from),
                        Profile::LegacyHttp => crate::http::timing_reply(&buf[..n], time, timestamp(origin.elapsed())).map(Vec::from),
                    }
                } else {
                    None
                };
                match reply {
                    Some(reply) => {
                        counts.requests.fetch_add(1, Ordering::Relaxed);
                        if socket.send_to(&reply,from).await.is_ok() {
                            counts.replies.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    None => { counts.ignored.fetch_add(1, Ordering::Relaxed); }
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }
    }
}
async fn timing_probes(
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    origin: Instant,
    mut stop: watch::Receiver<bool>,
) {
    for seq in 1u16..=3 {
        if *stop.borrow() {
            return;
        }
        let mut packet = [0; 32];
        packet[..2].copy_from_slice(&[0x80, 0xd2]);
        packet[2..4].copy_from_slice(&seq.to_be_bytes());
        packet[24..]
            .copy_from_slice(&(timestamp(origin.elapsed()) + (2208988800u64 << 32)).to_be_bytes());
        let _ = socket.send_to(&packet, peer).await;
        tokio::select! {_=stop.changed()=>return,_=tokio::time::sleep(Duration::from_millis(100))=>{}}
    }
}

pub(crate) async fn control_loop<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    mut control: Connection<S>,
    uri: String,
    stop: watch::Sender<bool>,
    mut cancelled: watch::Receiver<bool>,
    stats: watch::Sender<Stats>,
) -> (Option<Error>, bool) {
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut failure = None;
    loop {
        if *cancelled.borrow() {
            break;
        }
        tokio::select! {
            biased;
            _ = cancelled.changed() => break,
            _ = interval.tick() => {
                let result=tokio::select! {
                    biased;
                    _ = cancelled.changed() => break,
                    result = control.request("POST","/feedback","",&[],&[]) => result,
                };
                if let Err(error)=result {
                    failure=Some(error);stop.send_replace(true);break;
                }
                stats.send_modify(|s|s.feedback_replies+=1);
            }
        }
    }
    let teardown = matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            control.request("TEARDOWN", &uri, "", &[], &[])
        )
        .await,
        Ok(Ok(_))
    );
    (failure, teardown)
}

fn with_port(mut endpoint: SocketAddr, port: u16) -> SocketAddr {
    endpoint.set_port(port);
    endpoint
}
/// Cancelling a partial TCP packet ends this stream; cipher state is never reused.
pub(crate) async fn write_packet<S: tokio::io::AsyncWrite + Unpin>(
    socket: &mut S,
    packet: &[u8],
    stop: &mut watch::Receiver<bool>,
) -> Result<(), Error> {
    if *stop.borrow() {
        return Err(Error::Stopped);
    }
    tokio::select! {biased;
        _=stop.changed()=>Err(Error::Stopped),
        result=tokio::time::timeout(Duration::from_secs(1),socket.write_all(packet))=>result.map_err(|_|Error::Timeout)?.map_err(Into::into),
    }
}

/// Test-only: drive the real media engine over loopback TCP and return the
/// payload types of the first `count` packets the receiver side reads.
pub(crate) async fn engine_packet_types(
    delay_before_frame: Duration,
    access_unit: Vec<u8>,
    count: usize,
) -> Result<Vec<u8>, Error> {
    use tokio::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let authentication = TcpStream::connect(address).await?;
    let (_authentication_peer, _) = listener.accept().await?;
    let media = TcpStream::connect(address).await?;
    let (mut peer, _) = listener.accept().await?;
    let (read, data) = media.into_split();
    let ready = Ready {
        read: Some(read),
        lifecycle: Some(Lifecycle::Http {
            authentication: Connection::new(authentication),
        }),
        data,
        event: None,
        video: Video::http([1; 16], [2; 16], 1280, 720),
        audio: None,
        origin: Instant::now(),
        timing_task: None,
        timing_counts: Arc::default(),
    };
    let (stop, stop_rx) = watch::channel(false);
    let sender = launch(ready, Profile::LegacyHttp, stop, stop_rx, 90);
    tokio::time::sleep(delay_before_frame).await;
    sender.send_video(access_unit, true, Instant::now())?;
    let mut types = Vec::new();
    for _ in 0..count {
        let mut header = [0; 128];
        peer.read_exact(&mut header).await?;
        let len = u32::from_le_bytes(header[..4].try_into().expect("4 bytes")) as usize;
        if len > MAX_FRAME {
            return Err(Error::Protocol);
        }
        let mut payload = vec![0; len];
        peer.read_exact(&mut payload).await?;
        types.push(header[4]);
    }
    sender.close().await;
    Ok(types)
}
