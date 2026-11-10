//! Cast Streaming sender: one task owns the UDP socket and, per negotiated
//! stream, encrypts and packetizes frames, paces packets, sends RTCP sender
//! reports and acts on receiver feedback (checkpoint/ACK release, NACK
//! retransmission, picture-loss key-frame requests, kickstart). Behaviour
//! follows Open Screen `sender_impl.cc` / `sender_packet_router.cc`
//! (commit 308a18e24e717ce27231c0aaa713364a41416e54).
use crate::{
    offer::StreamOffer,
    rtcp,
    rtp::{self, EncryptedFrame, FrameCrypto, Packetizer},
};
use std::{
    collections::VecDeque,
    net::SocketAddr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::UdpSocket,
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior, interval},
};

/// Frame-ID span the 8-bit wire IDs can disambiguate (Open Screen kMaxUnackedFrames).
const MAX_UNACKED_FRAMES: i64 = 120;
const BURST_INTERVAL: Duration = Duration::from_millis(10);
/// 24 Mbps burst ceiling, as Open Screen's default packet router.
const PACKETS_PER_BURST: usize = 24_000_000 / 8 / 100 / rtp::MAX_PACKET_BYTES;
const REPORT_INTERVAL: Duration = Duration::from_millis(500);
/// Two 30 fps frames plus 15 ms capture jitter (Open Screen kMinSenderInFlight).
const MIN_IN_FLIGHT: Duration = Duration::from_micros(81_667);
const NEAR_ZERO_RTT: Duration = Duration::from_micros(75);
const FRAME_QUEUE: usize = 32;
const REPORT_HISTORY: usize = 32;

/// One encoded access unit (video) or audio packet, in RTP timebase ticks.
#[derive(Clone, Debug)]
pub struct EncodedFrame {
    pub key_frame: bool,
    pub rtp_timestamp: u32,
    pub data: Vec<u8>,
}

/// A negotiated stream: what was offered and the receiver's SSRC for it.
#[derive(Clone, Debug)]
pub struct StreamSpec {
    pub offer: StreamOffer,
    pub receiver_ssrc: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamStats {
    pub frames_enqueued: u64,
    pub frames_dropped: u64,
    /// Highest frame the receiver confirmed (all earlier ones too).
    pub checkpoint: i64,
    pub packets_sent: u64,
    pub packets_retransmitted: u64,
    pub round_trip_ms: u32,
    /// Increments when the encoder must produce a key frame next
    /// (receiver picture loss, or a frame had to be dropped).
    pub key_frame_requests: u64,
    pub feedback_received: u64,
}

#[derive(Debug)]
pub enum SendError {
    /// Queue full or streamer stopped; the encoder should skip this frame
    /// and send a key frame next.
    Busy,
    UnknownStream,
}

struct Input {
    stream: usize,
    frame: EncodedFrame,
}

/// Handle to a running sender. Dropping it stops streaming.
pub struct Streamer {
    frames: mpsc::Sender<Input>,
    stats: Vec<watch::Receiver<StreamStats>>,
    indexes: Vec<u32>,
    task: JoinHandle<()>,
}

impl Streamer {
    /// Binds a UDP socket connected to `receiver` and starts the sender task.
    pub async fn start(receiver: SocketAddr, streams: Vec<StreamSpec>) -> std::io::Result<Self> {
        let bind: SocketAddr = if receiver.is_ipv4() {
            "0.0.0.0:0".parse().expect("literal")
        } else {
            "[::]:0".parse().expect("literal")
        };
        let socket = UdpSocket::bind(bind).await?;
        socket.connect(receiver).await?;
        Ok(Self::spawn(socket, streams))
    }

    /// Runs over an already-connected socket (tests use loopback).
    pub fn spawn(socket: UdpSocket, streams: Vec<StreamSpec>) -> Self {
        let (frames, inbox) = mpsc::channel(FRAME_QUEUE);
        let mut stats = Vec::new();
        let mut tracks = Vec::new();
        let indexes = streams.iter().map(|spec| spec.offer.index).collect();
        for spec in streams {
            let (publish, subscribe) = watch::channel(StreamStats {
                checkpoint: -1,
                ..StreamStats::default()
            });
            stats.push(subscribe);
            tracks.push(Track::new(spec, publish));
        }
        let task = tokio::spawn(run(socket, tracks, inbox));
        Self {
            frames,
            stats,
            indexes,
            task,
        }
    }

    /// Position of the stream with offer `index` in this streamer.
    pub fn stream(&self, index: u32) -> Option<usize> {
        self.indexes
            .iter()
            .position(|candidate| *candidate == index)
    }

    /// Non-blocking. Frames must be offered in increasing RTP time.
    pub fn send(&self, stream: usize, frame: EncodedFrame) -> Result<(), SendError> {
        if stream >= self.stats.len() {
            return Err(SendError::UnknownStream);
        }
        self.frames
            .try_send(Input { stream, frame })
            .map_err(|_| SendError::Busy)
    }

    pub fn stats(&self, stream: usize) -> Option<watch::Receiver<StreamStats>> {
        self.stats.get(stream).cloned()
    }
}

impl Drop for Streamer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Pending {
    frame: EncryptedFrame,
    sent_at: Vec<Option<Instant>>,
    needs_send: Vec<bool>,
}

struct Track {
    spec: StreamSpec,
    crypto: FrameCrypto,
    packetizer: Packetizer,
    publish: watch::Sender<StreamStats>,
    stats: StreamStats,
    pending: VecDeque<Pending>,
    checkpoint: i64,
    last_enqueued: i64,
    latest_expected: i64,
    last_key_frame: i64,
    round_trip: Duration,
    report_rtp: u32,
    report_reference: Option<Instant>,
    octets: u32,
    reports: VecDeque<(u32, Instant)>,
    report_due: bool,
}

impl Track {
    fn new(spec: StreamSpec, publish: watch::Sender<StreamStats>) -> Self {
        let sequence = random_u16();
        let crypto = FrameCrypto::new(spec.offer.aes_key, spec.offer.aes_iv_mask);
        let packetizer = Packetizer::new(spec.offer.payload_type(), spec.offer.ssrc, sequence);
        Self {
            spec,
            crypto,
            packetizer,
            publish,
            stats: StreamStats {
                checkpoint: -1,
                ..StreamStats::default()
            },
            pending: VecDeque::new(),
            checkpoint: -1,
            last_enqueued: -1,
            latest_expected: -1,
            last_key_frame: -1,
            round_trip: Duration::ZERO,
            report_rtp: 0,
            report_reference: None,
            octets: 0,
            reports: VecDeque::new(),
            report_due: false,
        }
    }

    fn timebase(&self) -> u32 {
        self.spec.offer.timebase()
    }

    fn target_delay(&self) -> Duration {
        Duration::from_millis(u64::from(self.spec.offer.target_delay_ms))
    }

    fn max_in_flight(&self) -> Duration {
        let ceiling = MIN_IN_FLIGHT.max(self.target_delay() / 3);
        (self.round_trip * 2).clamp(MIN_IN_FLIGHT, ceiling)
    }

    fn request_key_frame(&mut self) {
        self.stats.key_frame_requests += 1;
    }

    fn enqueue(&mut self, frame: EncodedFrame, now: Instant) {
        let frame_id = self.last_enqueued + 1;
        let in_flight = self.pending.front().map_or(Duration::ZERO, |oldest| {
            let ticks = frame.rtp_timestamp.wrapping_sub(oldest.frame.rtp_timestamp);
            Duration::from_secs_f64(f64::from(ticks) / f64::from(self.timebase()))
        });
        let too_many = frame_id - self.checkpoint > MAX_UNACKED_FRAMES;
        let Some(packets) = rtp::packet_count(frame.data.len()) else {
            self.drop_frame();
            return;
        };
        // Dropping a predicted frame breaks the decode chain, so the encoder
        // is asked for a key frame. A key frame itself is never refused for
        // in-flight time: without it the receiver could never recover.
        if too_many || (!frame.key_frame && in_flight > self.max_in_flight()) {
            self.drop_frame();
            return;
        }
        let mut data = frame.data;
        self.crypto.crypt(frame_id, &mut data);
        let encrypted = EncryptedFrame {
            frame_id,
            referenced_frame_id: if frame.key_frame {
                frame_id
            } else {
                frame_id - 1
            },
            key_frame: frame.key_frame,
            rtp_timestamp: frame.rtp_timestamp,
            new_playout_delay_ms: None,
            data,
        };
        self.pending.push_back(Pending {
            frame: encrypted,
            sent_at: vec![None; packets],
            needs_send: vec![true; packets],
        });
        self.last_enqueued = frame_id;
        if frame.key_frame {
            self.last_key_frame = frame_id;
        }
        self.report_rtp = frame.rtp_timestamp;
        self.report_reference = Some(now);
        self.stats.frames_enqueued += 1;
        // Until a receiver report proves the receiver has lip-sync timing,
        // send a sender report before the media.
        if self.round_trip.is_zero() {
            self.report_due = true;
        }
    }

    fn drop_frame(&mut self) {
        self.stats.frames_dropped += 1;
        self.request_key_frame();
    }

    /// Next (frame, packet) needing a first send or a retransmit.
    fn next_packet(&self) -> Option<(usize, u16)> {
        self.pending.iter().enumerate().find_map(|(slot, pending)| {
            pending
                .needs_send
                .iter()
                .position(|needed| *needed)
                .map(|packet| (slot, packet as u16))
        })
    }

    /// The receiver may not know the newest frames exist if every packet of
    /// them was lost: resend the last packet after a quiet interval.
    fn kickstart(&self, now: Instant) -> Option<(usize, u16)> {
        if self.latest_expected >= self.last_enqueued {
            return None;
        }
        let slot = self.pending.len().checked_sub(1)?;
        let last = self.pending[slot].sent_at.len() - 1;
        let sent = self.pending[slot].sent_at[last]?;
        let wait = (self.target_delay() / 20).max(self.round_trip * 2);
        (now >= sent + wait).then_some((slot, last as u16))
    }

    fn build_packet(&mut self, slot: usize, packet: u16, now: Instant, out: &mut Vec<u8>) {
        let pending = &mut self.pending[slot];
        if pending.sent_at[usize::from(packet)].is_some() {
            self.stats.packets_retransmitted += 1;
        }
        self.packetizer.packet(&pending.frame, packet, out);
        pending.needs_send[usize::from(packet)] = false;
        pending.sent_at[usize::from(packet)] = Some(now);
        self.stats.packets_sent += 1;
        self.octets = self
            .octets
            .wrapping_add(out.len().saturating_sub(rtp::BASE_HEADER_BYTES) as u32);
    }

    fn sender_report(&mut self, now: Instant) -> Option<[u8; 28]> {
        let reference = self.report_reference?;
        let elapsed = now.saturating_duration_since(reference);
        let rtp = self
            .report_rtp
            .wrapping_add((elapsed.as_secs_f64() * f64::from(self.timebase())) as u32);
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let ntp = rtcp::ntp_timestamp(wall.as_secs(), wall.subsec_nanos());
        if self.reports.len() == REPORT_HISTORY {
            self.reports.pop_front();
        }
        self.reports.push_back((rtcp::report_id(ntp), now));
        self.report_due = false;
        Some(rtcp::sender_report(
            self.spec.offer.ssrc,
            ntp,
            rtp,
            self.stats.packets_sent as u32,
            self.octets,
        ))
    }

    fn feedback(&mut self, feedback: rtcp::Feedback, arrival: Instant) {
        self.stats.feedback_received += 1;
        if let Some(report) = feedback.report
            && let Some((_, sent)) = self
                .reports
                .iter()
                .find(|(id, _)| *id == report.last_report_id)
        {
            let held =
                Duration::from_secs_f64(f64::from(report.delay_since_last_report) / 65_536.0);
            let measurement = arrival
                .saturating_duration_since(*sent)
                .saturating_sub(held)
                .max(NEAR_ZERO_RTT)
                .min(self.target_delay());
            // Fast attack, slow decay (Open Screen SmoothRoundTripTime).
            self.round_trip = if self.round_trip.is_zero() {
                measurement
            } else if measurement > self.round_trip {
                (self.round_trip + measurement) / 2
            } else {
                (self.round_trip * 7 + measurement) / 8
            };
        }
        if let Some(checkpoint) = feedback.checkpoint
            && checkpoint <= self.last_enqueued
        {
            if checkpoint > self.checkpoint {
                self.checkpoint = checkpoint;
                self.pending
                    .retain(|pending| pending.frame.frame_id > checkpoint);
            }
            self.latest_expected = self.latest_expected.max(checkpoint);
        }
        if let Some(&newest) = feedback.acks.last()
            && newest <= self.last_enqueued
        {
            self.pending.retain(|pending| {
                feedback
                    .acks
                    .binary_search(&pending.frame.frame_id)
                    .is_err()
            });
            self.latest_expected = self.latest_expected.max(newest);
        }
        let too_recent = arrival.checked_sub(self.round_trip).unwrap_or(arrival);
        for nack in &feedback.nacks {
            let Some(pending) = self
                .pending
                .iter_mut()
                .find(|pending| pending.frame.frame_id == nack.frame_id)
            else {
                continue;
            };
            self.latest_expected = self.latest_expected.max(nack.frame_id);
            let packets = pending.sent_at.len();
            let range = if nack.packet_id == rtp::ALL_PACKETS_LOST {
                0..packets
            } else if usize::from(nack.packet_id) < packets {
                usize::from(nack.packet_id)..usize::from(nack.packet_id) + 1
            } else {
                continue;
            };
            for packet in range {
                // A NACK may have crossed a packet already in flight.
                if pending.sent_at[packet].is_none_or(|sent| sent <= too_recent) {
                    pending.needs_send[packet] = true;
                }
            }
        }
        // Repeated picture-loss reports are ignored while a key frame is in flight.
        if feedback.picture_loss && self.checkpoint >= self.last_key_frame {
            self.request_key_frame();
        }
        self.stats.checkpoint = self.checkpoint;
        self.stats.round_trip_ms = self.round_trip.as_millis() as u32;
    }

    fn publish(&self) {
        self.publish.send_if_modified(|current| {
            let changed = *current != self.stats;
            if changed {
                *current = self.stats.clone();
            }
            changed
        });
    }
}

async fn run(socket: UdpSocket, mut tracks: Vec<Track>, mut inbox: mpsc::Receiver<Input>) {
    let mut burst = interval(BURST_INTERVAL);
    burst.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut reports = interval(REPORT_INTERVAL);
    reports.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut datagram = vec![0_u8; 2048];
    let mut packet = Vec::with_capacity(rtp::MAX_PACKET_BYTES);
    loop {
        tokio::select! {
            input = inbox.recv() => {
                let Some(Input { stream, frame }) = input else { return };
                let now = Instant::now();
                if let Some(track) = tracks.get_mut(stream) {
                    track.enqueue(frame, now);
                    if track.report_due
                        && let Some(report) = track.sender_report(now)
                    {
                        let _ = socket.send(&report).await;
                    }
                    track.publish();
                }
            }
            received = socket.recv(&mut datagram) => {
                let Ok(length) = received else { continue };
                let arrival = Instant::now();
                let bytes = &datagram[..length];
                for track in &mut tracks {
                    if let Some(feedback) = rtcp::parse(
                        bytes,
                        track.spec.receiver_ssrc,
                        track.spec.offer.ssrc,
                        track.last_enqueued.max(0),
                    ) && feedback != rtcp::Feedback::default()
                    {
                        track.feedback(feedback, arrival);
                        track.publish();
                    }
                }
            }
            _ = burst.tick() => {
                let now = Instant::now();
                let mut budget = PACKETS_PER_BURST;
                // Audio streams are offered first; serve them first.
                for track in &mut tracks {
                    while budget > 0 {
                        let Some((slot, id)) = track.next_packet().or_else(|| track.kickstart(now)) else {
                            break;
                        };
                        track.build_packet(slot, id, now, &mut packet);
                        let _ = socket.send(&packet).await;
                        budget -= 1;
                    }
                    track.publish();
                }
            }
            _ = reports.tick() => {
                let now = Instant::now();
                for track in &mut tracks {
                    if let Some(report) = track.sender_report(now) {
                        let _ = socket.send(&report).await;
                    }
                }
            }
        }
    }
}

fn random_u16() -> u16 {
    let mut bytes = [0_u8; 2];
    let _ = rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut bytes);
    u16::from_be_bytes(bytes)
}

/// Fresh per-session secrets and SSRCs for an OFFER.
pub fn random_bytes16() -> [u8; 16] {
    let mut bytes = [0_u8; 16];
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut bytes)
        .expect("system random source");
    bytes
}

/// SSRC in Open Screen's ranges: audio 1..=50000 (higher priority), video
/// 50001..=100000.
pub fn random_ssrc(audio: bool) -> u32 {
    let mut bytes = [0_u8; 4];
    let _ = rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut bytes);
    let value = u32::from_be_bytes(bytes) % 50_000;
    if audio { 1 + value } else { 50_001 + value }
}
