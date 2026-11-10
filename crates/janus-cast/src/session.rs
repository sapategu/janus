//! App-owned session with one existing Cast receiver.
//!
//! The session connects, proves the receiver's device identity offline
//! (`trust`), opens the platform virtual connection, keeps it alive with
//! heartbeats and publishes the receiver's status and screen-mirroring
//! capability as a latest-value snapshot. Until [`Session::cast`] is called it
//! is read-only: the TV keeps showing whatever it shows. `cast` launches the
//! receiver's mirroring app, negotiates Cast Streaming (OFFER/ANSWER) and
//! returns a [`Streamer`] for encoded media; [`Session::stop_cast`] or closing
//! the session stops that app again. Losses are retried with bounded
//! backoff; trust, authentication and permission failures are final.
//! Dropping the handle closes the channel.
use crate::{
    connection,
    control::{self, AUDIO_MIRRORING_APP, AppAvailability, Event, SCREEN_MIRRORING_APP},
    offer::{self, StreamOffer, WEBRTC_NAMESPACE},
    streaming::{StreamSpec, Streamer},
    trust::{self, DevicePolicy},
    wire::{self, Message, Payload},
};
use std::{
    future::Future,
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::{Instant, sleep_until, timeout},
};

const MAX_ENDPOINTS: usize = 8;
const READ_QUEUE: usize = 16;
const COMMAND_QUEUE: usize = 4;
/// Consecutive unparseable receiver messages tolerated before reconnecting.
const MAX_INVALID_MESSAGES: u32 = 32;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
const STATUS_REQUEST: u32 = 1;
const AVAILABILITY_REQUEST: u32 = 2;

/// Timing policy. Defaults follow Chromium: 5 s heartbeat, 10 s liveness.
#[derive(Clone, Copy, Debug)]
pub struct SessionConfig {
    pub ping_interval: Duration,
    pub liveness_timeout: Duration,
    /// Reconnect attempts after a loss before giving up.
    pub max_reconnects: u32,
    pub reconnect_backoff: Duration,
    /// Bound for each cast step (app launch, then ANSWER). A receiver may
    /// need to switch input and start its mirroring app.
    pub cast_timeout: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            ping_interval: Duration::from_secs(5),
            liveness_timeout: Duration::from_secs(10),
            max_reconnects: 3,
            reconnect_backoff: Duration::from_secs(1),
            cast_timeout: Duration::from_secs(20),
        }
    }
}

/// Whether a receiver reports it can run a mirroring app. Device policy
/// overrides it: an audio-only device never offers screen mirroring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    Unknown,
    Available,
    Unavailable,
}

impl From<AppAvailability> for Capability {
    fn from(value: AppAvailability) -> Self {
        match value {
            AppAvailability::Available => Self::Available,
            AppAvailability::Unavailable => Self::Unavailable,
            AppAvailability::Unknown => Self::Unknown,
        }
    }
}

/// Why a connected channel was lost (the session then reconnects).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loss {
    /// No message within the liveness timeout.
    Timeout,
    /// The receiver closed the virtual connection.
    ReceiverClosed,
    /// TCP/TLS ended or failed.
    Disconnected,
    /// Too many malformed receiver messages.
    Protocol,
}

/// Final failures; the session stops retrying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    NoAddress,
    NetworkDenied,
    Unreachable,
    TlsRejected,
    AuthenticationRejected,
    Untrusted(trust::Error),
    /// Reconnects exhausted after repeated losses.
    Lost(Loss),
}

/// Why starting or continuing a cast failed.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CastError {
    #[error("the receiver is not connected")]
    NotConnected,
    #[error("a cast is already running")]
    AlreadyCasting,
    #[error("this receiver can't show a screen")]
    ScreenUnsupported,
    #[error("the receiver couldn't start screen mirroring: {0}")]
    LaunchFailed(String),
    #[error("the receiver didn't respond in time")]
    Timeout,
    #[error("the receiver declined the stream: {0}")]
    Rejected(String),
    #[error("the network connection to the receiver failed")]
    Network,
    #[error("nothing to cast")]
    NoStreams,
}

/// Cast state shown alongside the connection phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CastPhase {
    Idle,
    Launching,
    Negotiating,
    Streaming,
    /// Stopped from Janus.
    Stopped,
    /// The receiver ended mirroring (e.g. its remote was used).
    EndedByReceiver,
    Failed(CastError),
}

/// A connected, trust-verified stream and the receiver's IP for media.
pub struct Connected<S> {
    pub stream: S,
    pub policy: DevicePolicy,
    pub peer: IpAddr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Connecting,
    Connected,
    Reconnecting { attempt: u32 },
    Failed(Failure),
    Closed,
}

/// Latest receiver state. Plain data, safe to render; no identifiers.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// Increments on every successful (re)connection.
    pub generation: u64,
    pub phase: Phase,
    pub policy: Option<DevicePolicy>,
    /// An app is running on the receiver (it may be busy).
    pub receiver_app_active: Option<bool>,
    pub volume: Option<f64>,
    pub muted: Option<bool>,
    pub screen_mirroring: Capability,
    pub audio_mirroring: Capability,
    pub last_loss: Option<Loss>,
    /// Heartbeat replies received in this generation.
    pub heartbeats: u64,
    pub cast: CastPhase,
}

impl Snapshot {
    fn new() -> Self {
        Self {
            generation: 0,
            phase: Phase::Connecting,
            policy: None,
            receiver_app_active: None,
            volume: None,
            muted: None,
            screen_mirroring: Capability::Unknown,
            audio_mirroring: Capability::Unknown,
            last_loss: None,
            heartbeats: 0,
            cast: CastPhase::Idle,
        }
    }

    /// Connected and the receiver reported it can mirror a screen.
    pub fn ready_for_screen(&self) -> bool {
        self.phase == Phase::Connected
            && self.policy == Some(DevicePolicy::Unrestricted)
            && self.screen_mirroring == Capability::Available
    }
}

enum Command {
    Refresh,
    Cast {
        streams: Vec<StreamOffer>,
        reply: oneshot::Sender<Result<Streamer, CastError>>,
    },
    StopCast,
    Close,
}

/// Handle to a running session. Dropping it aborts the task and closes the
/// channel; [`Session::close`] first sends a graceful virtual-connection CLOSE.
pub struct Session {
    commands: mpsc::Sender<Command>,
    snapshot: watch::Receiver<Snapshot>,
    task: Option<JoinHandle<()>>,
}

impl Session {
    /// Start a verified session to the receiver at one of `endpoints`.
    pub fn start(
        endpoints: Vec<SocketAddr>,
        config: SessionConfig,
        runtime: &tokio::runtime::Handle,
    ) -> Self {
        if endpoints.is_empty() || endpoints.len() > MAX_ENDPOINTS {
            return Self::finished(Failure::NoAddress);
        }
        Self::spawn(runtime, config, move || {
            let endpoints = endpoints.clone();
            async move {
                let (channel, device) = connection::connect_verified(&endpoints).await?;
                let peer = channel
                    .get_ref()
                    .0
                    .peer_addr()
                    .map_err(|_| connection::Error::Connect)?;
                Ok(Connected {
                    stream: channel,
                    policy: device.policy(),
                    peer: peer.ip(),
                })
            }
        })
    }

    fn finished(failure: Failure) -> Self {
        let mut snapshot = Snapshot::new();
        snapshot.phase = Phase::Failed(failure);
        let (_, snapshot) = watch::channel(snapshot);
        let (commands, _) = mpsc::channel(1);
        Self {
            commands,
            snapshot,
            task: None,
        }
    }

    fn spawn<S, F, Fut>(runtime: &tokio::runtime::Handle, config: SessionConfig, connect: F) -> Self
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Connected<S>, connection::Error>> + Send + 'static,
    {
        let (publish, snapshot) = watch::channel(Snapshot::new());
        let (commands, inbox) = mpsc::channel(COMMAND_QUEUE);
        let task = runtime.spawn(supervise(config, connect, publish, inbox));
        Self {
            commands,
            snapshot,
            task: Some(task),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.borrow().clone()
    }

    /// Latest-value updates; intermediate states may be coalesced.
    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.snapshot.clone()
    }

    /// Ask the receiver for fresh status. Dropped if a request is queued.
    pub fn refresh(&self) {
        let _ = self.commands.try_send(Command::Refresh);
    }

    /// Explicit user Cast: launches the receiver's mirroring app, negotiates
    /// `streams` and returns the media sender. Video requires an unrestricted
    /// device whose receiver offers screen mirroring.
    pub async fn cast(&self, streams: Vec<StreamOffer>) -> Result<Streamer, CastError> {
        if streams.is_empty() {
            return Err(CastError::NoStreams);
        }
        let snapshot = self.snapshot();
        if snapshot.phase != Phase::Connected {
            return Err(CastError::NotConnected);
        }
        let video = streams
            .iter()
            .any(|stream| matches!(stream.media, offer::Media::Video { .. }));
        if video && !snapshot.ready_for_screen() {
            return Err(CastError::ScreenUnsupported);
        }
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Cast { streams, reply })
            .await
            .map_err(|_| CastError::NotConnected)?;
        answer.await.unwrap_or(Err(CastError::Network))
    }

    /// Explicit user Stop: stops the mirroring app Janus launched.
    pub fn stop_cast(&self) {
        let _ = self.commands.try_send(Command::StopCast);
    }

    /// Graceful close: stops any cast, sends CLOSE, then ends the task.
    pub async fn close(mut self) {
        let _ = self.commands.try_send(Command::Close);
        if let Some(task) = self.task.take() {
            let abort = task.abort_handle();
            if timeout(CLOSE_TIMEOUT, task).await.is_err() {
                abort.abort();
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn failure(error: &connection::Error) -> Failure {
    use connection::Error as E;
    match error {
        E::Address => Failure::NoAddress,
        E::NetworkDenied => Failure::NetworkDenied,
        E::Tls => Failure::TlsRejected,
        E::Authentication(_) => Failure::AuthenticationRejected,
        E::Untrusted(error) => Failure::Untrusted(*error),
        E::Timeout | E::Connect | E::Wire(_) | E::MessageLimit => Failure::Unreachable,
    }
}

enum Outcome {
    Lost(Loss),
    Closed,
}

async fn supervise<S, F, Fut>(
    config: SessionConfig,
    mut connect: F,
    publish: watch::Sender<Snapshot>,
    mut commands: mpsc::Receiver<Command>,
) where
    S: AsyncRead + AsyncWrite + Send + 'static,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Connected<S>, connection::Error>>,
{
    let mut attempt = 0;
    loop {
        let result = tokio::select! {
            result = connect() => result,
            command = commands.recv() => {
                match command {
                    Some(Command::Close) | None => {
                        publish.send_modify(|s| s.phase = Phase::Closed);
                        return;
                    }
                    Some(Command::Cast { reply, .. }) => {
                        let _ = reply.send(Err(CastError::NotConnected));
                    }
                    _ => {}
                }
                continue;
            }
        };
        match result {
            Ok(Connected {
                stream,
                policy,
                peer,
            }) => {
                publish.send_modify(|s| {
                    s.generation += 1;
                    s.phase = Phase::Connected;
                    s.policy = Some(policy);
                    s.heartbeats = 0;
                    s.receiver_app_active = None;
                    s.screen_mirroring = Capability::Unknown;
                    s.audio_mirroring = Capability::Unknown;
                });
                let outcome = run(stream, policy, peer, &config, &publish, &mut commands).await;
                publish.send_modify(|s| {
                    if matches!(
                        s.cast,
                        CastPhase::Launching | CastPhase::Negotiating | CastPhase::Streaming
                    ) {
                        s.cast = CastPhase::Failed(CastError::Network);
                    }
                });
                match outcome {
                    Outcome::Closed => {
                        publish.send_modify(|s| s.phase = Phase::Closed);
                        return;
                    }
                    Outcome::Lost(loss) => {
                        // Only a generation that proved two-way liveness (a
                        // heartbeat reply) refills the retry budget; a receiver
                        // that accepts and then goes silent cannot flap forever.
                        if publish.borrow().heartbeats > 0 {
                            attempt = 0;
                        }
                        publish.send_modify(|s| s.last_loss = Some(loss));
                        if attempt >= config.max_reconnects {
                            publish.send_modify(|s| s.phase = Phase::Failed(Failure::Lost(loss)));
                            return;
                        }
                    }
                }
            }
            Err(error) if !error.is_retryable() => {
                publish.send_modify(|s| s.phase = Phase::Failed(failure(&error)));
                return;
            }
            Err(error) => {
                if attempt >= config.max_reconnects {
                    let final_failure = publish
                        .borrow()
                        .last_loss
                        .map_or(failure(&error), Failure::Lost);
                    publish.send_modify(|s| s.phase = Phase::Failed(final_failure));
                    return;
                }
            }
        }
        attempt += 1;
        publish.send_modify(|s| s.phase = Phase::Reconnecting { attempt });
        let backoff = config.reconnect_backoff * 2_u32.saturating_pow(attempt - 1).min(8);
        tokio::select! {
            () = tokio::time::sleep(backoff) => {}
            command = commands.recv() => match command {
                Some(Command::Close) | None => {
                    publish.send_modify(|s| s.phase = Phase::Closed);
                    return;
                }
                Some(Command::Cast { reply, .. }) => {
                    let _ = reply.send(Err(CastError::NotConnected));
                }
                _ => {}
            }
        }
    }
}

/// A cast in progress inside one connected generation.
enum Casting {
    Launching {
        request: u32,
        app: &'static str,
        deadline: Instant,
        streams: Vec<StreamOffer>,
        reply: Option<oneshot::Sender<Result<Streamer, CastError>>>,
    },
    Negotiating {
        transport: String,
        app_session: String,
        sequence: u32,
        deadline: Instant,
        streams: Vec<StreamOffer>,
        reply: Option<oneshot::Sender<Result<Streamer, CastError>>>,
    },
    Streaming {
        transport: String,
        app_session: String,
    },
}

impl Casting {
    fn deadline(&self) -> Option<Instant> {
        match self {
            Self::Launching { deadline, .. } | Self::Negotiating { deadline, .. } => {
                Some(*deadline)
            }
            Self::Streaming { .. } => None,
        }
    }

    fn app_session(&self) -> Option<(&str, &str)> {
        match self {
            Self::Launching { .. } => None,
            Self::Negotiating {
                transport,
                app_session,
                ..
            }
            | Self::Streaming {
                transport,
                app_session,
            } => Some((transport, app_session)),
        }
    }
}

/// Writes one message; `false` means the channel is gone.
async fn send<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: Result<Message, control::Error>,
) -> bool {
    match message {
        Ok(message) => wire::write(writer, &message).await.is_ok(),
        Err(_) => true,
    }
}

/// One connected generation. Reads run in a separate task because framed
/// reads are not cancellation-safe inside `select!`.
async fn run<S: AsyncRead + AsyncWrite + Send + 'static>(
    stream: S,
    policy: DevicePolicy,
    peer: IpAddr,
    config: &SessionConfig,
    publish: &watch::Sender<Snapshot>,
    commands: &mut mpsc::Receiver<Command>,
) -> Outcome {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (forward, mut inbound) = mpsc::channel(READ_QUEUE);
    let reader_task = tokio::spawn(async move {
        while let Ok(message) = wire::read(&mut reader).await {
            if forward.send(message).await.is_err() {
                break;
            }
        }
    });
    let outcome = async {
        let opening = [
            control::connect(),
            control::get_status(STATUS_REQUEST),
            control::get_app_availability(
                AVAILABILITY_REQUEST,
                &[SCREEN_MIRRORING_APP, AUDIO_MIRRORING_APP],
            ),
        ];
        for message in opening {
            let Ok(message) = message else {
                return Outcome::Lost(Loss::Protocol);
            };
            if wire::write(&mut writer, &message).await.is_err() {
                return Outcome::Lost(Loss::Disconnected);
            }
        }
        let mut next_request = AVAILABILITY_REQUEST;
        let mut next_sequence = 0_u32;
        let mut last_seen = Instant::now();
        let mut next_ping = last_seen + config.ping_interval;
        let mut invalid = 0;
        let mut casting: Option<Casting> = None;
        let set_cast = |phase: CastPhase| publish.send_modify(|s| s.cast = phase);
        loop {
            let mut deadline = (last_seen + config.liveness_timeout).min(next_ping);
            if let Some(cast_deadline) = casting.as_ref().and_then(Casting::deadline) {
                deadline = deadline.min(cast_deadline);
            }
            tokio::select! {
                message = inbound.recv() => {
                    let Some(message) = message else {
                        return Outcome::Lost(Loss::Disconnected);
                    };
                    last_seen = Instant::now();
                    // Traffic from the launched app's transport (OFFER/ANSWER,
                    // its virtual-connection CLOSE) is not receiver control.
                    if message.source != wire::RECEIVER {
                        let from_app = casting
                            .as_ref()
                            .and_then(Casting::app_session)
                            .is_some_and(|(transport, _)| transport == message.source);
                        if !from_app {
                            continue;
                        }
                        if message.namespace == wire::CONNECTION_NAMESPACE {
                            // The mirroring app closed its connection.
                            casting = None;
                            set_cast(CastPhase::EndedByReceiver);
                            continue;
                        }
                        if message.namespace != WEBRTC_NAMESPACE {
                            continue;
                        }
                        let Payload::Text(text) = &message.payload else { continue };
                        if let Some(Casting::Negotiating { transport, app_session, sequence, streams, reply, .. }) =
                            casting.as_mut()
                        {
                            match offer::parse_answer(text, *sequence, streams) {
                                Err(offer::AnswerError::NotAnswer) => {}
                                Err(error) => {
                                    let (transport, app_session) = (transport.clone(), app_session.clone());
                                    if let Some(reply) = reply.take() {
                                        let _ = reply.send(Err(CastError::Rejected(error.to_string())));
                                    }
                                    next_request = next_request.wrapping_add(1).max(3);
                                    let _ = send(&mut writer, control::stop(next_request, &app_session)).await;
                                    let _ = send(&mut writer, control::close_to(&transport)).await;
                                    casting = None;
                                    set_cast(CastPhase::Failed(CastError::Rejected(error.to_string())));
                                }
                                Ok(answer) => {
                                    let specs: Vec<StreamSpec> = answer
                                        .send_indexes
                                        .iter()
                                        .zip(&answer.ssrcs)
                                        .filter_map(|(index, ssrc)| {
                                            streams.iter().find(|s| s.index == *index).map(|offer| StreamSpec {
                                                offer: offer.clone(),
                                                receiver_ssrc: *ssrc,
                                            })
                                        })
                                        .collect();
                                    let result = Streamer::start(SocketAddr::new(peer, answer.udp_port), specs)
                                        .await
                                        .map_err(|_| CastError::Network);
                                    let ok = result.is_ok();
                                    let delivered = reply.take().is_some_and(|reply| reply.send(result).is_ok());
                                    let (transport, app_session) = (transport.clone(), app_session.clone());
                                    if ok && delivered {
                                        casting = Some(Casting::Streaming { transport, app_session });
                                        set_cast(CastPhase::Streaming);
                                    } else {
                                        // The caller gave up or UDP failed: undo the launch.
                                        next_request = next_request.wrapping_add(1).max(3);
                                        let _ = send(&mut writer, control::stop(next_request, &app_session)).await;
                                        let _ = send(&mut writer, control::close_to(&transport)).await;
                                        casting = None;
                                        set_cast(if ok { CastPhase::Stopped } else { CastPhase::Failed(CastError::Network) });
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    match control::parse(&message, None) {
                        Ok(Event::Ping) => {
                            if !send(&mut writer, control::pong()).await {
                                return Outcome::Lost(Loss::Disconnected);
                            }
                        }
                        Ok(Event::Pong) => publish.send_modify(|s| s.heartbeats += 1),
                        Ok(Event::Closed) => return Outcome::Lost(Loss::ReceiverClosed),
                        Ok(Event::Status { status, .. }) => {
                            publish.send_modify(|s| {
                                s.receiver_app_active = Some(status.has_active_application());
                                s.volume = status.volume.and_then(|v| v.level);
                                s.muted = status.volume.and_then(|v| v.muted);
                            });
                            match casting.take() {
                                Some(Casting::Launching { app, streams, mut reply, request, deadline }) => {
                                    let launched = status.applications.iter().find(|a| a.app_id == app);
                                    if let Some(launched) = launched {
                                        let transport = launched.transport_id.clone();
                                        let app_session = launched.session_id.clone();
                                        next_sequence = next_sequence.wrapping_add(1).max(1);
                                        let text = offer::offer_message(next_sequence, &streams);
                                        let offer_message =
                                            Message::new(wire::SENDER, &transport, WEBRTC_NAMESPACE, Payload::Text(text))
                                                .map_err(control::Error::from);
                                        if !send(&mut writer, control::connect_to(&transport)).await
                                            || !send(&mut writer, offer_message).await
                                        {
                                            if let Some(reply) = reply.take() {
                                                let _ = reply.send(Err(CastError::Network));
                                            }
                                            return Outcome::Lost(Loss::Disconnected);
                                        }
                                        set_cast(CastPhase::Negotiating);
                                        casting = Some(Casting::Negotiating {
                                            transport,
                                            app_session,
                                            sequence: next_sequence,
                                            deadline: Instant::now() + config.cast_timeout,
                                            streams,
                                            reply,
                                        });
                                    } else {
                                        casting = Some(Casting::Launching { app, streams, reply, request, deadline });
                                    }
                                }
                                Some(Casting::Streaming { transport, app_session }) => {
                                    if status.applications.iter().any(|a| a.session_id == app_session) {
                                        casting = Some(Casting::Streaming { transport, app_session });
                                    } else {
                                        set_cast(CastPhase::EndedByReceiver);
                                    }
                                }
                                other => casting = other,
                            }
                        }
                        Ok(Event::LaunchError { request_id, reason }) => {
                            if let Some(Casting::Launching { request, reply, .. }) = casting.as_mut()
                                && request_id.is_none_or(|id| id == *request)
                            {
                                if let Some(reply) = reply.take() {
                                    let _ = reply.send(Err(CastError::LaunchFailed(reason.clone())));
                                }
                                casting = None;
                                set_cast(CastPhase::Failed(CastError::LaunchFailed(reason)));
                            }
                        }
                        Ok(Event::AppAvailability { apps, .. }) => publish.send_modify(|s| {
                            for (app, state) in &apps {
                                let state = Capability::from(*state);
                                if app == SCREEN_MIRRORING_APP {
                                    s.screen_mirroring = if policy == DevicePolicy::AudioOnly {
                                        Capability::Unavailable
                                    } else {
                                        state
                                    };
                                } else if app == AUDIO_MIRRORING_APP {
                                    s.audio_mirroring = state;
                                }
                            }
                        }),
                        Ok(Event::Other) => {}
                        Err(_) => {
                            invalid += 1;
                            if invalid > MAX_INVALID_MESSAGES {
                                return Outcome::Lost(Loss::Protocol);
                            }
                            continue;
                        }
                    }
                    invalid = 0;
                }
                () = sleep_until(deadline) => {
                    let now = Instant::now();
                    if now >= last_seen + config.liveness_timeout {
                        return Outcome::Lost(Loss::Timeout);
                    }
                    if casting.as_ref().and_then(Casting::deadline).is_some_and(|d| now >= d) {
                        let session = casting.as_ref().and_then(Casting::app_session)
                            .map(|(t, a)| (t.to_owned(), a.to_owned()));
                        if let Some(Casting::Launching { reply, .. } | Casting::Negotiating { reply, .. }) = casting.as_mut()
                            && let Some(reply) = reply.take()
                        {
                            let _ = reply.send(Err(CastError::Timeout));
                        }
                        if let Some((transport, app_session)) = session {
                            next_request = next_request.wrapping_add(1).max(3);
                            let _ = send(&mut writer, control::stop(next_request, &app_session)).await;
                            let _ = send(&mut writer, control::close_to(&transport)).await;
                        }
                        casting = None;
                        set_cast(CastPhase::Failed(CastError::Timeout));
                    }
                    if now >= next_ping {
                        next_ping = now + config.ping_interval;
                        if !send(&mut writer, control::ping()).await {
                            return Outcome::Lost(Loss::Disconnected);
                        }
                    }
                }
                command = commands.recv() => match command {
                    Some(Command::Refresh) => {
                        next_request = next_request.wrapping_add(1).max(3);
                        if !send(&mut writer, control::get_status(next_request)).await {
                            return Outcome::Lost(Loss::Disconnected);
                        }
                    }
                    Some(Command::Cast { streams, reply }) => {
                        if casting.is_some() {
                            let _ = reply.send(Err(CastError::AlreadyCasting));
                            continue;
                        }
                        let video = streams.iter().any(|s| matches!(s.media, offer::Media::Video { .. }));
                        let app = if video { SCREEN_MIRRORING_APP } else { AUDIO_MIRRORING_APP };
                        next_request = next_request.wrapping_add(1).max(3);
                        if !send(&mut writer, control::launch(next_request, app)).await {
                            let _ = reply.send(Err(CastError::Network));
                            return Outcome::Lost(Loss::Disconnected);
                        }
                        set_cast(CastPhase::Launching);
                        casting = Some(Casting::Launching {
                            request: next_request,
                            app,
                            deadline: Instant::now() + config.cast_timeout,
                            streams,
                            reply: Some(reply),
                        });
                    }
                    Some(Command::StopCast) => {
                        let session = casting.as_ref().and_then(Casting::app_session)
                            .map(|(t, a)| (t.to_owned(), a.to_owned()));
                        if let Some((transport, app_session)) = session {
                            next_request = next_request.wrapping_add(1).max(3);
                            let _ = send(&mut writer, control::stop(next_request, &app_session)).await;
                            let _ = send(&mut writer, control::close_to(&transport)).await;
                        }
                        if casting.take().is_some() {
                            set_cast(CastPhase::Stopped);
                        }
                    }
                    Some(Command::Close) | None => {
                        let session = casting.as_ref().and_then(Casting::app_session)
                            .map(|(t, a)| (t.to_owned(), a.to_owned()));
                        let _ = timeout(CLOSE_TIMEOUT, async {
                            if let Some((transport, app_session)) = session {
                                let _ = send(&mut writer, control::stop(next_request.wrapping_add(1).max(3), &app_session)).await;
                                let _ = send(&mut writer, control::close_to(&transport)).await;
                            }
                            let _ = send(&mut writer, control::close()).await;
                            let _ = writer.shutdown().await;
                        })
                        .await;
                        if casting.is_some() {
                            set_cast(CastPhase::Stopped);
                        }
                        return Outcome::Closed;
                    }
                },
            }
        }
    }
    .await;
    reader_task.abort();
    outcome
}

/// Session over caller-supplied streams for integration tests. The
/// connector chooses the policy, so this is never device trust; production
/// code uses [`Session::start`].
#[doc(hidden)]
pub mod testing {
    use super::*;

    pub fn start_with<S, F, Fut>(
        runtime: &tokio::runtime::Handle,
        config: SessionConfig,
        connect: F,
    ) -> Session
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Connected<S>, connection::Error>> + Send + 'static,
    {
        Session::spawn(runtime, config, connect)
    }
}
