//! `janus cast`: discover → protocol session → screen/window/test-pattern
//! cast with optional desktop audio → live stats → clean stop. The same
//! session and pipeline code the desktop app uses.
use anyhow::{Context, Result, bail};
use janus_discovery::{Candidate, ReceiverProtocol};
use janus_mirror::{Options, Source};
use std::time::Duration;
use tokio::time::Instant;

pub struct CastArgs {
    pub name: Option<String>,
    /// Only this protocol, for a TV advertised under one name by several.
    pub protocol: Option<ReceiverProtocol>,
    pub source: Source,
    pub audio: bool,
    pub seconds: Option<u64>,
    pub resolution: janus_mirror::Resolution,
    pub latency_ms: Option<u32>,
    pub bit_rate: u32,
    pub frame_rate: u32,
}

/// Picks the receiver: by exact name, or the only supported video-capable receiver.
pub fn choose(candidates: &[Candidate], name: Option<&str>) -> Result<Candidate> {
    choose_protocol(candidates, name, None)
}
/// As [`choose`], optionally restricted to one protocol.
pub fn choose_protocol(
    candidates: &[Candidate],
    name: Option<&str>,
    protocol: Option<ReceiverProtocol>,
) -> Result<Candidate> {
    let cast: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| janus_mirror::supports_receiver(c))
        .filter(|c| protocol.is_none_or(|p| c.protocol == p))
        .collect();
    if let Some(name) = name {
        let named: Vec<_> = cast.iter().filter(|c| c.display_name == name).collect();
        return match named.as_slice() {
            [] => bail!("no supported screen named {name:?} found"),
            [only] => Ok((***only).clone()),
            _ => bail!(
                "several destinations share the name {name:?}; add --protocol airplay or --protocol cast, or choose it in the Janus desktop app"
            ),
        };
    }
    match cast.as_slice() {
        [] => bail!("no supported screen found on this network"),
        [only] => Ok((*only).clone()),
        many => bail!(
            "several screens found; choose one with --name: {}",
            many.iter()
                .map(|c| format!("{:?}", c.display_name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Some receivers' advertisements are intermittently missing from a single
/// browse window (observed on a TV whose AirPlay SRV target is
/// `localhost.local`), so a named target gets up to three windows.
async fn find(name: Option<&str>, protocol: Option<ReceiverProtocol>) -> Result<Candidate> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let candidates = janus_discovery::browse(Duration::from_secs(4)).await?;
        match choose_protocol(&candidates, name, protocol) {
            Err(_) if name.is_some() && attempt < 3 => continue,
            result => return result,
        }
    }
}

pub async fn run(args: CastArgs) -> Result<()> {
    eprintln!("Looking for screens…");
    let target = find(args.name.as_deref(), args.protocol).await?;
    eprintln!("Connecting to {}…", target.display_name);
    let options = Options {
        source: args.source,
        audio: args.audio,
        frame_rate: args.frame_rate,
        ..Options::default().with_resolution(args.resolution)
    };
    let options = Options {
        video_bit_rate: args.bit_rate,
        latency_ms: args.latency_ms,
        ..options
    };
    match args.source {
        Source::Screen => eprintln!("Choose the screen to share in the system dialog…"),
        Source::Window => eprintln!("Choose the window to share in the system dialog…"),
        Source::TestPattern => {}
    }
    let casting = tokio::select! {
        result=janus_mirror::start_receiver(&target,options)=>result.context("couldn't start casting")?,
        _=tokio::signal::ctrl_c()=>return Ok(()),
    };
    let mut stats = casting.stats();
    let size = stats.borrow().clone();
    eprintln!(
        "Casting to {} at {}×{} {} fps ({} encoder, {:.1} Mbit/s){}. Press Ctrl+C to stop.",
        target.display_name,
        size.width,
        size.height,
        size.frame_rate,
        if size.gpu_encoder { "GPU" } else { "software" },
        f64::from(size.bit_rate) / 1e6,
        if args.audio {
            " with sound"
        } else {
            " without sound"
        }
    );

    let deadline = args
        .seconds
        .map(|s| Instant::now() + Duration::from_secs(s));
    let mut report = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            () = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => break,
            _ = report.tick() => {
                let current = stats.borrow_and_update().clone();
                if let Some(reason) = &current.ended {
                    eprintln!("{reason}");
                    break;
                }
                eprintln!(
                    "video {} frames ({} key, {:.1} MB), audio {} packets, receiver checkpoint {} (−1 means unavailable), RTT {} ms, encode {} ms{}{}",
                    current.video_frames,
                    current.key_frames,
                    current.video_bytes as f64 / 1e6,
                    current.audio_packets,
                    current.receiver_checkpoint,
                    current.round_trip_ms,
                    current.encode_ms,
                    if current.dropped_frames > 0 { format!(", {} dropped", current.dropped_frames) } else { String::new() },
                    if current.late_frames > 0 { format!(", {} late", current.late_frames) } else { String::new() },
                );
            }
        }
    }
    eprintln!("Stopping…");
    casting.stop().await;
    Ok(())
}

/// `janus pair`: one-time AirPlay PIN pairing. The TV shows a code; the user
/// types it here. The resulting pairing is stored locally for later casts.
pub async fn pair(name: &str) -> Result<()> {
    eprintln!("Looking for {name:?}…");
    let target = find(Some(name), Some(ReceiverProtocol::AirPlay)).await?;
    let pairing = janus_airplay::begin_pin_pairing(&target.endpoints)
        .await
        .context("couldn't start pairing")?;
    eprintln!("Type the code shown on {name} and press Enter:");
    let pin = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map(|_| line)
    })
    .await??;
    pairing.finish(pin.trim()).await.context("pairing failed")?;
    eprintln!("Paired with {name}. You can now cast to it.");
    Ok(())
}
