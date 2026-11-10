//! Generic receiver acceptance: discover every receiver on the LAN and run
//! the protocol's staged checks against each one. Nothing here names a device;
//! any TV or screen that appears is validated the same way. Read-only by
//! default; with `mirror` set, each video-capable receiver is sent a test
//! pattern and must acknowledge the frames.
use anyhow::Result;
use janus_cast::session::{Capability, Failure, Phase, Session, SessionConfig, Snapshot};
use janus_cast::trust::DevicePolicy;
use janus_discovery::{Candidate, ReceiverProtocol};
use serde::Serialize;
use std::time::Duration;
use tokio::time::{Instant, timeout};

const MAX_RECEIVERS: usize = 16;
const CONNECT_BUDGET: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "result", content = "detail")]
pub enum Outcome {
    Pass(String),
    Fail(String),
    /// The stage is not implemented yet for this protocol.
    Pending(String),
}

#[derive(Clone, Debug, Serialize)]
pub struct Check {
    pub stage: &'static str,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReceiverReport {
    pub name: String,
    pub model: Option<String>,
    pub protocol: ReceiverProtocol,
    pub endpoints: usize,
    pub checks: Vec<Check>,
}

impl ReceiverReport {
    pub fn failed(&self) -> bool {
        self.checks
            .iter()
            .any(|check| matches!(check.outcome, Outcome::Fail(_)))
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub discover_for: Duration,
    /// How long to hold each live session to prove heartbeats keep it alive.
    pub hold: Duration,
    /// Optional exact display-name filter (targeting one unit, never required).
    pub name: Option<String>,
    /// Cast a test pattern for this long and require receiver ACKs. `None`
    /// keeps the run read-only (nothing is launched on the TV).
    pub mirror: Option<Duration>,
}

pub async fn check_receivers(options: &Options) -> Result<Vec<ReceiverReport>> {
    let mut candidates = janus_discovery::browse(options.discover_for).await?;
    if let Some(name) = &options.name {
        candidates.retain(|candidate| &candidate.display_name == name);
    }
    candidates.truncate(MAX_RECEIVERS);
    let mut tasks = tokio::task::JoinSet::new();
    for (index, candidate) in candidates.into_iter().enumerate() {
        let (hold, mirror) = (options.hold, options.mirror);
        tasks.spawn(async move { (index, check_one(candidate, hold, mirror).await) });
    }
    let mut reports = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        reports.push(joined?);
    }
    reports.sort_by_key(|(index, _)| *index);
    Ok(reports.into_iter().map(|(_, report)| report).collect())
}

async fn check_one(
    candidate: Candidate,
    hold: Duration,
    mirror: Option<Duration>,
) -> ReceiverReport {
    let mut checks = vec![Check {
        stage: "discovery",
        outcome: discovery(&candidate),
    }];
    match candidate.protocol {
        ReceiverProtocol::GoogleCast => checks.extend(google_cast(&candidate, hold, mirror).await),
        ReceiverProtocol::AirPlay => checks.extend(airplay(&candidate, mirror).await),
        ReceiverProtocol::JanusMoq => checks.push(Check {
            stage: "session",
            outcome: Outcome::Pending("Janus-native adapter checks not implemented".into()),
        }),
    }
    ReceiverReport {
        name: candidate.display_name,
        model: candidate.model,
        protocol: candidate.protocol,
        endpoints: candidate.endpoints.len(),
        checks,
    }
}

fn discovery(candidate: &Candidate) -> Outcome {
    if candidate.display_name.is_empty() {
        Outcome::Fail("receiver advertised no name".into())
    } else if candidate.endpoints.is_empty() {
        Outcome::Fail("no private LAN endpoint advertised".into())
    } else {
        Outcome::Pass(format!(
            "{} endpoint(s); advertised video={:?} audio={:?}",
            candidate.endpoints.len(),
            candidate.advertised_outputs.video,
            candidate.advertised_outputs.audio
        ))
    }
}

/// Stages: device trust → live control session (status + heartbeats over
/// `hold`) → mirroring capability → mirroring (pending).
async fn google_cast(
    candidate: &Candidate,
    hold: Duration,
    mirror: Option<Duration>,
) -> Vec<Check> {
    let config = SessionConfig::default();
    let session = Session::start(
        candidate.endpoints.clone(),
        config,
        &tokio::runtime::Handle::current(),
    );
    let mut updates = session.subscribe();
    let first = timeout(CONNECT_BUDGET, async {
        loop {
            let snapshot = updates.borrow_and_update().clone();
            let settled = match snapshot.phase {
                Phase::Connected => {
                    snapshot.receiver_app_active.is_some()
                        && snapshot.screen_mirroring != Capability::Unknown
                }
                Phase::Failed(_) | Phase::Closed => true,
                _ => false,
            };
            if settled || updates.changed().await.is_err() {
                return snapshot;
            }
        }
    })
    .await
    .unwrap_or_else(|_| session.snapshot());

    let mut checks = Vec::new();
    if let Phase::Failed(failure) = first.phase {
        let reason = match failure {
            Failure::Untrusted(error) => format!("not a genuine Cast device: {error}"),
            other => format!("{other:?}"),
        };
        let stage = if matches!(
            failure,
            Failure::Untrusted(_) | Failure::AuthenticationRejected | Failure::TlsRejected
        ) {
            "device-trust"
        } else {
            "connect"
        };
        checks.push(Check {
            stage,
            outcome: Outcome::Fail(reason),
        });
        return checks;
    }
    if first.phase != Phase::Connected {
        checks.push(Check {
            stage: "connect",
            outcome: Outcome::Fail(format!("no session within budget ({:?})", first.phase)),
        });
        return checks;
    }
    checks.push(Check {
        stage: "device-trust",
        outcome: Outcome::Pass(match first.policy {
            Some(DevicePolicy::AudioOnly) => "genuine Cast device, audio-only policy".into(),
            _ => "genuine Cast device".into(),
        }),
    });
    checks.push(Check {
        stage: "capability",
        outcome: capability(candidate, &first),
    });

    let held = hold_session(&session, hold).await;
    checks.push(Check {
        stage: "session",
        outcome: held,
    });
    let mirroring = match mirror {
        None => {
            Outcome::Pending("read-only run; add --mirror <seconds> to cast a test pattern".into())
        }
        Some(_) if !first.ready_for_screen() => {
            Outcome::Pass("not a screen receiver; mirroring not applicable".into())
        }
        Some(duration) => mirror_test_pattern(&session, duration).await,
    };
    session.close().await;
    checks.push(Check {
        stage: "mirroring",
        outcome: mirroring,
    });
    checks
}

/// Casts the test pattern for `duration` and requires the receiver to have
/// acknowledged at least half the frames sent (its RTCP checkpoint).
async fn mirror_test_pattern(session: &Session, duration: Duration) -> Outcome {
    let options = janus_mirror::Options {
        source: janus_mirror::Source::TestPattern,
        audio: false,
        video_bit_rate: 3_000_000,
        ..janus_mirror::Options::default()
    };
    let mirror = match janus_mirror::start(session, options).await {
        Ok(mirror) => mirror,
        Err(error) => return Outcome::Fail(format!("cast did not start: {error}")),
    };
    tokio::time::sleep(duration).await;
    let stats = mirror.stats().borrow().clone();
    mirror.stop(session).await;
    if let Some(reason) = stats.ended {
        return Outcome::Fail(format!("cast ended early: {reason}"));
    }
    let acked = stats.receiver_checkpoint + 1;
    if stats.video_frames == 0 || (acked as u64) * 2 < stats.video_frames {
        return Outcome::Fail(format!(
            "receiver acknowledged {acked} of {} frames",
            stats.video_frames
        ));
    }
    Outcome::Pass(format!(
        "{}×{} test pattern: {} frames sent, {acked} acknowledged, RTT {} ms",
        stats.width, stats.height, stats.video_frames, stats.round_trip_ms
    ))
}

fn capability(candidate: &Candidate, snapshot: &Snapshot) -> Outcome {
    let advertised_video = candidate.advertised_outputs.video;
    match (snapshot.policy, snapshot.screen_mirroring) {
        (Some(DevicePolicy::AudioOnly), _) if advertised_video == Some(true) => Outcome::Fail(
            "advertises video but its certificate is audio-only; must not be offered a screen"
                .into(),
        ),
        (_, Capability::Available) => Outcome::Pass("screen mirroring app available".into()),
        (_, Capability::Unavailable) => {
            Outcome::Pass("receiver reports no screen mirroring app; offer audio or nothing".into())
        }
        (_, Capability::Unknown) => {
            Outcome::Fail("receiver did not answer app availability".into())
        }
    }
}

async fn hold_session(session: &Session, hold: Duration) -> Outcome {
    let start = session.snapshot();
    let deadline = Instant::now() + hold;
    let mut updates = session.subscribe();
    while Instant::now() < deadline {
        let _ = timeout(deadline - Instant::now(), updates.changed()).await;
        let now = updates.borrow_and_update().clone();
        if now.generation != start.generation || now.phase != Phase::Connected {
            return Outcome::Fail(format!(
                "session dropped during hold: {:?}, last loss {:?}",
                now.phase, now.last_loss
            ));
        }
    }
    let end = session.snapshot();
    let ping = SessionConfig::default().ping_interval;
    let expected = (hold.as_secs() / ping.as_secs().max(1)).saturating_sub(1);
    if end.heartbeats < expected {
        return Outcome::Fail(format!(
            "only {} heartbeat replies in {}s (expected ≥{expected})",
            end.heartbeats,
            hold.as_secs()
        ));
    }
    Outcome::Pass(format!(
        "held {}s, {} heartbeat replies, receiver app active={:?}",
        hold.as_secs(),
        end.heartbeats,
        end.receiver_app_active
    ))
}

async fn airplay(candidate: &Candidate, mirror: Option<Duration>) -> Vec<Check> {
    let mut checks = Vec::new();
    let capability = match janus_airplay::probe(&candidate.endpoints).await {
        Ok(capability) => capability,
        Err(error) => {
            checks.push(Check {
                stage: "capability",
                outcome: Outcome::Fail(error.to_string()),
            });
            return checks;
        }
    };
    checks.push(Check {
        stage: "capability",
        outcome: Outcome::Pass(format!(
            "live {}: screen={} supported audio={}; canvas ≤{}×{}",
            capability.profile.label(),
            capability.screen,
            capability.audio,
            capability.width,
            capability.height
        )),
    });
    checks.push(Check {
        stage: "profile",
        outcome: Outcome::Pass(capability.profile.label().into()),
    });
    let hisense = capability.profile == janus_airplay::Profile::Hisense;
    let hisense_audio = hisense && capability.audio;
    if !capability.screen {
        checks.push(Check {
            stage: "mirroring",
            outcome: Outcome::Pass(
                "receiver does not advertise screen mirroring; not applicable".into(),
            ),
        });
        return checks;
    }
    let Some(duration) = mirror else {
        for stage in ["authentication", "session", "mirroring"] {
            checks.push(Check{stage,outcome:Outcome::Pending("read-only run; use --mirror <seconds> for explicit receiver negotiation and test-pattern transmission".into())});
        }
        if hisense {
            checks.push(Check {
                stage: "audio",
                outcome: Outcome::Pending(if hisense_audio {
                    "read-only run; use --mirror <seconds> to test desktop sound transmission"
                        .into()
                } else {
                    "receiver audio support unavailable; no sound transmission attempted".into()
                }),
            });
        }
        return checks;
    };
    let options = janus_mirror::Options {
        source: janus_mirror::Source::TestPattern,
        audio: hisense_audio,
        video_bit_rate: 3_000_000,
        ..janus_mirror::Options::default()
    };
    let casting = match janus_mirror::start_receiver(candidate, options).await {
        Ok(casting) => casting,
        Err(error) => {
            checks.push(Check {
                stage: "session",
                outcome: Outcome::Fail(error.to_string()),
            });
            return checks;
        }
    };
    let stats = casting.stats();
    let profile = stats.borrow().airplay_profile;
    checks.push(Check {
        stage: "authentication",
        outcome: airplay_authentication(profile),
    });
    let http = profile == Some(janus_airplay::Profile::LegacyHttp);
    checks.push(Check {
        stage: "session",
        outcome: Outcome::Pass(if http {
            "HTTP mirroring transport established; encrypted stream preamble sent; this profile has no start ACK".into()
        } else { "SETUP and activation accepted".into() }),
    });
    tokio::time::sleep(duration).await;
    let before = stats.borrow().clone();
    let stopped = casting.stop().await;
    let sent = stopped.transmitted_video_frames;
    let clock = format!(
        "timing {} requests / {} replies / {} other datagrams ignored",
        stopped.timing_requests, stopped.timing_replies, stopped.timing_ignored
    );
    let receiver = if stopped.receiver_events.is_empty() {
        "receiver: no reply or close observed".to_owned()
    } else {
        format!("receiver: {}", stopped.receiver_events.join(", "))
    };
    let mirroring = if let Some(reason) = before.ended {
        let after = stopped
            .stream_ended_after_ms
            .map_or_else(String::new, |ms| format!(" {ms} ms after stream start"));
        Outcome::Fail(format!(
            "sharing ended early{after}: {reason}; {} frames queued / {sent} transmitted; {clock}; {receiver}",
            before.video_frames
        ))
    } else if sent == 0 {
        Outcome::Fail(format!("no complete video frames transmitted; {clock}"))
    } else {
        Outcome::Pending(format!(
            "{}×{}: {} frames queued / {sent} transmitted; {clock}; no per-frame ACK; confirm rendering physically",
            before.width, before.height, before.video_frames
        ))
    };
    checks.push(Check {
        stage: "mirroring",
        outcome: mirroring,
    });
    if hisense_audio {
        checks.push(Check {
            stage: "audio",
            outcome: hisense_audio_transmission(stopped.transmitted_audio_packets),
        });
    }
    if http {
        checks.push(Check {
            stage: "timing",
            outcome: if stopped.timing_requests == 0 {
                Outcome::Pending("no legacy NTP request observed; timing not confirmed".into())
            } else if stopped.timing_replies == 0 {
                Outcome::Fail("legacy NTP requests received but no replies sent".into())
            } else {
                Outcome::Pass(clock)
            },
        });
    }
    checks.push(Check {
        stage: "teardown",
        outcome: airplay_teardown(profile, stopped.receiver_stop_confirmed),
    });
    checks
}

/// Authentication evidence for the transport that actually started.
#[doc(hidden)]
pub fn airplay_authentication(profile: Option<janus_airplay::Profile>) -> Outcome {
    match profile {
        Some(janus_airplay::Profile::Hisense) => Outcome::Pending(
            "EShare video transport is unencrypted and does not authenticate receiver identity; no FairPlay or pairing proof".into(),
        ),
        Some(janus_airplay::Profile::Paired) => Outcome::Pass(
            "stored HomeKit pairing verified; encrypted control and media established".into(),
        ),
        Some(janus_airplay::Profile::Rtsp | janus_airplay::Profile::LegacyHttp) => Outcome::Pass(
            "legacy key-possession proof and FairPlay exchange completed; no manufacturer-identity claim".into(),
        ),
        None => Outcome::Pending("transport authentication evidence unavailable".into()),
    }
}

/// Stop evidence distinguishes protocol acknowledgements from local closure.
#[doc(hidden)]
pub fn airplay_teardown(
    profile: Option<janus_airplay::Profile>,
    confirmed: Option<bool>,
) -> Outcome {
    match profile {
        Some(janus_airplay::Profile::Hisense) => Outcome::Pending(
            "local EShare stream/control closed; no RTSP teardown acknowledgement; confirm display stopped".into(),
        ),
        Some(janus_airplay::Profile::LegacyHttp) => Outcome::Pending(
            "local HTTP stream and timing service closed; this profile has no teardown ACK; confirm the display stopped".into(),
        ),
        _ if confirmed == Some(true) => {
            Outcome::Pass("receiver acknowledged TEARDOWN".into())
        }
        _ => Outcome::Fail(
            "local sharing stopped, but receiver teardown was not acknowledged".into(),
        ),
    }
}

/// Sent audio packets establish transport activity, not audible playback.
#[doc(hidden)]
pub fn hisense_audio_transmission(packets: u64) -> Outcome {
    if packets == 0 {
        Outcome::Fail("no complete audio packets transmitted".into())
    } else {
        Outcome::Pending(format!(
            "{packets} audio packets transmitted; no audio playback ACK; confirm sound physically"
        ))
    }
}
