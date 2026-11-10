use janus_cli::fixture;
use janus_session::control;

use anyhow::{Result, ensure};
use bytes::Bytes;
use clap::{Parser, Subcommand};
use janus_media::{
    clock::{ClockEstimator, Probe},
    quality::{Adaptation, Decision, Feedback},
    video::{Frame, VideoQueue},
};
use janus_session::Source;
use janus_wire::Code;
use std::{
    net::{SocketAddr, SocketAddrV4},
    time::Duration,
};

#[derive(Parser)]
#[command(about = "Janus casting services and diagnostics. Native media integration is pending.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Discover compatible existing TVs and optional Janus receivers.
    Discover {
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=60))]
        seconds: u64,
    },
    /// Cast this computer's screen, one window, or a test pattern to a TV.
    Cast {
        /// Exact screen name (needed when several are found).
        #[arg(long)]
        name: Option<String>,
        /// Only this protocol, when a TV is listed under one name by several.
        #[arg(long, value_parser = ["airplay", "cast"])]
        protocol: Option<String>,
        /// What to share: screen, window or test-pattern.
        #[arg(long, default_value = "screen", value_parser = ["screen", "window", "test-pattern"])]
        source: String,
        /// Don't send the computer's sound.
        #[arg(long)]
        no_audio: bool,
        /// Stop automatically after this many seconds.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=86400))]
        seconds: Option<u64>,
        /// Largest output size: auto (the screen's own size, up to what the TV
        /// supports), 720p, 1080p, 2k (1440p) or 4k (2160p). Never upscales.
        #[arg(long, default_value = "auto", value_parser = ["auto", "720p", "1080p", "1440p", "2k", "2160p", "4k"])]
        resolution: String,
        /// Video bitrate in Mbit/s (default: chosen by --resolution).
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=60))]
        mbps: Option<u32>,
        /// Receiver playout delay in milliseconds (default: per protocol).
        #[arg(long, value_parser = clap::value_parser!(u32).range(40..=2000))]
        latency_ms: Option<u32>,
        /// Frames per second (the receiver and encoder may deliver fewer).
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(5..=60))]
        fps: u32,
    },
    /// Pair once with an AirPlay TV that shows a code on screen.
    Pair {
        /// Exact screen name.
        #[arg(long)]
        name: String,
    },
    /// Explicit synthetic checks; these never open screen capture.
    Diagnostics {
        #[command(subcommand)]
        command: Diagnostic,
    },
}

#[derive(Subcommand)]
enum Diagnostic {
    /// Write deterministic Y4M video, PCM impulses and their shared event timeline.
    Fixture {
        output: std::path::PathBuf,
        #[arg(long, default_value_t = 180, value_parser = clap::value_parser!(u32).range(120..=3600))]
        frames: u32,
    },
    /// Correlate external instrument timestamps; retain misses and signed skew.
    Correlate {
        observations: std::path::PathBuf,
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=10000))]
        expected_events: u32,
        #[arg(long, value_parser = clap::value_parser!(u32).range(0..=1000000))]
        calibration_error_us: u32,
    },
    /// Exercise a real encrypted loopback synthetic session and cleanup.
    SelfTest,
    /// Listen for synthetic control sessions. No screen/audio capture.
    Serve {
        #[arg(long, default_value = "127.0.0.1:45831")]
        bind: SocketAddr,
        #[arg(long)]
        advertise: bool,
    },
    /// Start, heartbeat and stop a synthetic session on a diagnostic receiver.
    Probe { address: SocketAddr },
    /// Verify an existing Cast receiver's device identity, read-only: sends
    /// only the auth challenge, verifies offline, closes. Never casts or
    /// interrupts the TV. Address example: 192.168.1.20:8009.
    CastCheck { address: SocketAddr },
    /// Discover every receiver on the LAN and run each protocol's staged,
    /// read-only acceptance checks. Exits non-zero on any failure.
    CheckReceivers {
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=60))]
        seconds: u64,
        /// Seconds to hold each live session (heartbeat check).
        #[arg(long, default_value_t = 12, value_parser = clap::value_parser!(u64).range(0..=600))]
        hold: u64,
        /// Only check receivers with this exact display name.
        #[arg(long)]
        name: Option<String>,
        /// Also cast a test pattern for this many seconds to each screen and
        /// require it to acknowledge the frames (shows on the TV).
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=120))]
        mirror: Option<u64>,
    },
    /// Drive deadline/quality/clock policies on an artificial timeline.
    Simulate {
        #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u32).range(1..=216000))]
        frames: u32,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Discover { seconds } => {
            let candidates = janus_discovery::browse(Duration::from_secs(seconds)).await?;
            println!("{}", serde_json::to_string_pretty(&candidates)?);
            Ok(())
        }
        Command::Diagnostics { command } => diagnostics(command).await,
        Command::Pair { name } => janus_cli::cast::pair(&name).await,
        Command::Cast {
            name,
            protocol,
            source,
            no_audio,
            seconds,
            resolution,
            latency_ms,
            mbps,
            fps,
        } => {
            let resolution =
                janus_mirror::Resolution::parse(&resolution).expect("clap restricts --resolution");
            janus_cli::cast::run(janus_cli::cast::CastArgs {
                name,
                protocol: protocol.map(|p| match p.as_str() {
                    "airplay" => janus_discovery::ReceiverProtocol::AirPlay,
                    _ => janus_discovery::ReceiverProtocol::GoogleCast,
                }),
                source: match source.as_str() {
                    "window" => janus_mirror::Source::Window,
                    "test-pattern" => janus_mirror::Source::TestPattern,
                    _ => janus_mirror::Source::Screen,
                },
                audio: !no_audio,
                seconds,
                resolution,
                latency_ms,
                bit_rate: mbps.map_or(0, |m| m * 1_000_000),
                frame_rate: fps,
            })
            .await
        }
    }
}

async fn diagnostics(command: Diagnostic) -> Result<()> {
    match command {
        Diagnostic::Fixture { output, frames } => fixture::generate(&output, frames)?,
        Diagnostic::Correlate {
            observations,
            expected_events,
            calibration_error_us,
        } => {
            fixture::correlate(
                &observations,
                expected_events as usize,
                calibration_error_us,
            )?;
        }
        Diagnostic::SelfTest => {
            let server = control::Server::start("127.0.0.1:0".parse()?).await?;
            let result = probe(server.address).await;
            let idle = !server.state.lock().await.snapshot().output_permitted;
            server.shutdown().await?;
            result?;
            ensure!(idle, "output survived stop");
            println!(
                "PASS: QUIC/TLS 1.3 + Protobuf synthetic start/heartbeat/stop; no output lease remains. Native media not tested."
            );
        }
        Diagnostic::Serve { bind, advertise } => {
            let server = control::Server::start(bind).await?;
            println!(
                "Janus diagnostic receiver listening on {}. No capture or real media. Ctrl+C stops.",
                server.address
            );
            let mut advertiser = None;
            let (stop, stopped) = tokio::sync::watch::channel(false);
            // Diagnostics never advertise native casting readiness.
            let (_ready, availability) = tokio::sync::watch::channel(false);
            if advertise {
                ensure!(
                    matches!(server.address, SocketAddr::V4(address) if !address.ip().is_loopback()),
                    "advertising requires a concrete private IPv4 LAN address"
                );
                let SocketAddr::V4(address) = server.address else {
                    unreachable!()
                };
                let label = format!("Janus-Check-{}", address.port());
                advertiser = Some(tokio::spawn(janus_discovery::advertise(
                    label,
                    SocketAddrV4::new(*address.ip(), address.port()),
                    availability,
                    stopped,
                )));
            }
            let mut ticks = tokio::time::interval(Duration::from_millis(100));
            loop {
                tokio::select! {
                    result = tokio::signal::ctrl_c() => { result?; break; }
                    _ = ticks.tick() => {
                        if advertiser.as_ref().is_some_and(|task| task.is_finished()) {
                            let task = advertiser.take().unwrap();
                            match task.await { Ok(Ok(())) => eprintln!("Discovery advertisement ended"), Ok(Err(error)) => eprintln!("Discovery unavailable: {error}"), Err(error) => eprintln!("Discovery task failed: {error}") }
                        }
                    }
                }
            }
            server.state.lock().await.local_stop();
            let _ = stop.send(true);
            if let Some(task) = advertiser {
                let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
            }
            server.shutdown().await?;
        }
        Diagnostic::Probe { address } => probe(address).await?,
        Diagnostic::CastCheck { address } => cast_check(address).await?,
        Diagnostic::CheckReceivers {
            seconds,
            hold,
            name,
            mirror,
        } => {
            let reports = janus_cli::acceptance::check_receivers(&janus_cli::acceptance::Options {
                discover_for: Duration::from_secs(seconds),
                hold: Duration::from_secs(hold),
                name,
                mirror: mirror.map(Duration::from_secs),
            })
            .await?;
            println!("{}", serde_json::to_string_pretty(&reports)?);
            ensure!(
                !reports.is_empty(),
                "no receivers discovered on this network"
            );
            ensure!(
                !reports.iter().any(|report| report.failed()),
                "one or more receivers failed acceptance"
            );
        }
        Diagnostic::Simulate { frames } => simulate(frames)?,
    }
    Ok(())
}

async fn cast_check(address: SocketAddr) -> Result<()> {
    use janus_cast::trust::{DevicePolicy, RevocationSource};
    let device = janus_cast::connection::probe_authentication(&[address]).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "device_verified": true,
            "policy": match device.policy() {
                DevicePolicy::Unrestricted => "unrestricted",
                DevicePolicy::AudioOnly => "audio-only",
            },
            "revocation": match device.revocation() {
                RevocationSource::DeviceCrl => "device-crl",
                RevocationSource::FallbackCrl => "embedded-fallback-crl",
            },
            "control_or_media_sent": false,
        }))?
    );
    Ok(())
}

async fn probe(address: SocketAddr) -> Result<()> {
    let mut client = control::Client::connect(address).await?;
    ensure!(
        client.offer(Source::Synthetic).await? == Code::Ok,
        "synthetic session rejected"
    );
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        ensure!(client.heartbeat().await? == Code::Ok, "heartbeat rejected");
    }
    ensure!(client.stop().await? == Code::Ok, "stop rejected");
    println!("Synthetic session completed. No screen/audio media was sent.");
    Ok(())
}

fn simulate(frames: u32) -> Result<()> {
    let mut queue = VideoQueue::new(1);
    let mut adaptation = Adaptation::default();
    let mut estimator = ClockEstimator::default();
    let mut delivered = 0;
    let mut dropped = 0;
    let mut peak_bytes = 0;
    let mut changes = 0;
    let mut clock = None;
    for n in 0..frames {
        let now = 1_000_000 + u64::from(n) * 16_667;
        let pressure = (600..900).contains(&(n % 1800));
        if n % 12 == 0
            && matches!(
                adaptation.update(
                    now,
                    Feedback {
                        useful_bps: if pressure { 2_000_000 } else { 15_000_000 },
                        queue_age_us: if pressure { 60_000 } else { 0 },
                        late_fraction: 0.0,
                        audio_at_risk: false
                    }
                ),
                Decision::Change(_)
            )
        {
            changes += 1;
        }
        if n % 30 == 0 {
            let sender = |t: u64| (t as f64 * 1.0002 + 50_000.0).round() as u64;
            clock = Some(estimator.push(Probe {
                receiver_send: now,
                sender_receive: sender(now + 1000),
                sender_send: sender(now + 1100),
                receiver_receive: now + 2100,
            })?);
        }
        let result = queue.push(
            Frame {
                generation: 1,
                group: u64::from(n / 30),
                object: u64::from(n % 30),
                idr: n % 30 == 0,
                deadline_us: now + 30_000,
                payload: Bytes::from(vec![0; 16_384]),
            },
            now,
        );
        peak_bytes = peak_bytes.max(queue.queued_bytes());
        if result != janus_media::video::Admission::Accepted {
            dropped += 1;
        }
        if !pressure && queue.pop(now + 1_000).is_some() {
            delivered += 1;
        }
    }
    let clock = clock.unwrap();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "synthetic_only": true, "frames": frames, "popped_access_units": delivered, "admission_drops": dropped,
            "peak_queue_bytes": peak_bytes, "quality_changes": changes, "final_profile": adaptation.profile(),
            "fitted_drift_ppm": (clock.slope - 1.0) * 1_000_000.0,
            "clock_uncertainty_us": clock.uncertainty_us,
            "physical_latency_measured": false
        }))?
    );
    Ok(())
}
