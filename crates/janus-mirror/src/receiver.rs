//! Shared entry point used by CLI and GPUI. Protocol sessions remain owned by
//! their adapters; source capture/encoding stays in the common pipeline.
use crate::{Mirror, MirrorError, MirrorStats, Options};
use janus_cast::session::{Capability, Phase, Session, SessionConfig};
use janus_discovery::{Candidate, ReceiverProtocol};
use std::time::Duration;
use tokio::sync::watch;

pub struct Casting {
    session: Option<Session>,
    mirror: Mirror,
}
impl Casting {
    pub fn stats(&self) -> watch::Receiver<MirrorStats> {
        self.mirror.stats()
    }
    pub async fn stop(self) -> MirrorStats {
        let stats = self.mirror.stats();
        if let Some(session) = self.session {
            self.mirror.stop(&session).await;
            session.close().await;
        } else {
            self.mirror.shutdown().await;
        }
        stats.borrow().clone()
    }
}
pub fn supports_receiver(candidate: &Candidate) -> bool {
    matches!(
        candidate.protocol,
        ReceiverProtocol::GoogleCast | ReceiverProtocol::AirPlay
    ) && candidate.advertised_outputs.video != Some(false)
}
pub async fn start_receiver(
    candidate: &Candidate,
    options: Options,
) -> Result<Casting, MirrorError> {
    if !supports_receiver(candidate) {
        return Err(MirrorError::UnsupportedReceiver);
    }
    if candidate.protocol == ReceiverProtocol::AirPlay {
        return Ok(Casting {
            session: None,
            mirror: crate::start_airplay(&candidate.endpoints, options).await?,
        });
    }
    let session = Session::start(
        candidate.endpoints.clone(),
        SessionConfig::default(),
        &tokio::runtime::Handle::current(),
    );
    let mut updates = session.subscribe();
    let ready = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let snapshot = updates.borrow_and_update().clone();
            match snapshot.phase {
                Phase::Connected if snapshot.screen_mirroring != Capability::Unknown => {
                    return if snapshot.ready_for_screen() {
                        Ok(())
                    } else {
                        Err(MirrorError::UnsupportedReceiver)
                    };
                }
                Phase::Failed(failure) => {
                    return Err(MirrorError::Connection(format!("{failure:?}")));
                }
                Phase::Closed => {
                    return Err(MirrorError::Connection(
                        "The receiver connection ended.".into(),
                    ));
                }
                _ => {}
            }
            if updates.changed().await.is_err() {
                return Err(MirrorError::Connection(
                    "The receiver connection ended.".into(),
                ));
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        Err(MirrorError::Connection(
            "The receiver didn't answer in time.".into(),
        ))
    });
    if let Err(error) = ready {
        session.close().await;
        return Err(error);
    }
    match crate::start(&session, options).await {
        Ok(mirror) => Ok(Casting {
            session: Some(session),
            mirror,
        }),
        Err(error) => {
            session.close().await;
            Err(error)
        }
    }
}
