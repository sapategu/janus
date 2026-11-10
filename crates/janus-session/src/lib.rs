//! Single-owner receiver lifecycle. All times are session-monotonic microseconds.
//! The transport must bind calls to its connection; DNS labels confer no rights.

pub mod control;
/// Control-protocol state machine; exposed for integration tests only.
#[doc(hidden)]
pub mod protocol;
/// QUIC/TLS configuration; exposed for integration tests only.
#[doc(hidden)]
pub mod tls;

use serde::Serialize;
use thiserror::Error;

pub const HEARTBEAT_US: u64 = 500_000;
pub const OUTPUT_LEASE_US: u64 = 2_000_000;
pub const RECOVERY_US: u64 = 10_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Phase {
    Idle,
    Starting,
    Casting,
    Recovering,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Source {
    Synthetic,
    Video,
    AudioOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    pub session: [u8; 16],
    pub generation: u64,
    pub source: Source,
}

#[derive(Debug)]
struct Active {
    grant: Grant,
    last_sequence: u64,
    lease_until: u64,
    recover_until: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub receiving: bool,
    pub phase: Phase,
    pub generation: u64,
    pub source: Option<Source>,
    pub output_permitted: bool,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("receiving is disabled")]
    Disabled,
    #[error("receiver already has an active session")]
    Busy,
    #[error("stale session, generation or sequence")]
    Stale,
    #[error("session lease expired")]
    Expired,
    #[error("source is not supported by this backend")]
    Unsupported,
    #[error("invalid state or monotonic time")]
    Invalid,
}

/// The real backend must pass only sources that it can actually implement.
pub struct Coordinator {
    receiving: bool,
    phase: Phase,
    generation: u64,
    now: u64,
    supported: Vec<Source>,
    active: Option<Active>,
}

impl Coordinator {
    /// Current session-monotonic time in microseconds.
    pub fn now(&self) -> u64 {
        self.now
    }

    pub fn new(supported: Vec<Source>) -> Self {
        Self {
            receiving: false,
            phase: Phase::Idle,
            generation: 0,
            now: 0,
            supported,
            active: None,
        }
    }

    pub fn set_receiving(&mut self, enabled: bool) {
        self.receiving = enabled;
        if !enabled {
            self.clear();
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            receiving: self.receiving,
            phase: self.phase,
            generation: self.generation,
            source: self.active.as_ref().map(|active| active.grant.source),
            output_permitted: self.phase == Phase::Casting && self.active.is_some(),
        }
    }

    pub fn tick(&mut self, now: u64) -> Result<(), SessionError> {
        if now < self.now {
            return Err(SessionError::Invalid);
        }
        self.now = now;
        if let Some(active) = &self.active {
            if now >= active.recover_until {
                self.clear();
            } else if now >= active.lease_until {
                self.phase = Phase::Recovering;
            }
        }
        Ok(())
    }

    pub fn offer(
        &mut self,
        session: [u8; 16],
        source: Source,
        sequence: u64,
        now: u64,
    ) -> Result<Grant, SessionError> {
        self.tick(now)?;
        if !self.receiving {
            return Err(SessionError::Disabled);
        }
        if self.active.is_some() {
            return Err(SessionError::Busy);
        }
        if sequence == 0 || session == [0; 16] {
            return Err(SessionError::Invalid);
        }
        if !self.supported.contains(&source) {
            return Err(SessionError::Unsupported);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(SessionError::Invalid)?;
        let grant = Grant {
            session,
            generation: self.generation,
            source,
        };
        self.active = Some(Active {
            grant,
            last_sequence: sequence,
            lease_until: now.saturating_add(OUTPUT_LEASE_US),
            recover_until: now.saturating_add(RECOVERY_US),
        });
        self.phase = Phase::Starting;
        Ok(grant)
    }

    /// Called only after actual output startup. It never renews an expired lease.
    pub fn started(&mut self, grant: Grant, now: u64) -> Result<(), SessionError> {
        self.tick(now)?;
        self.check(grant)?;
        if self.phase != Phase::Starting {
            return Err(SessionError::Expired);
        }
        self.phase = Phase::Casting;
        Ok(())
    }

    pub fn heartbeat(&mut self, grant: Grant, sequence: u64, now: u64) -> Result<(), SessionError> {
        self.tick(now)?;
        self.check(grant)?;
        if self.phase == Phase::Recovering {
            return Err(SessionError::Expired);
        }
        let active = self.active.as_mut().ok_or(SessionError::Stale)?;
        if sequence <= active.last_sequence {
            return Err(SessionError::Stale);
        }
        active.last_sequence = sequence;
        active.lease_until = now.saturating_add(OUTPUT_LEASE_US);
        active.recover_until = now.saturating_add(RECOVERY_US);
        Ok(())
    }

    /// Only the same authenticated control connection may resume its old grant.
    /// A full reconnect requires a new offer and connection-bound authorization.
    pub fn resume(&mut self, grant: Grant, sequence: u64, now: u64) -> Result<Grant, SessionError> {
        self.tick(now)?;
        self.check(grant)?;
        if self.phase != Phase::Recovering {
            return Err(SessionError::Invalid);
        }
        let active = self.active.as_ref().ok_or(SessionError::Stale)?;
        if sequence <= active.last_sequence {
            return Err(SessionError::Stale);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(SessionError::Invalid)?;
        let next = Grant {
            generation: self.generation,
            ..grant
        };
        self.active = Some(Active {
            grant: next,
            last_sequence: sequence,
            lease_until: now.saturating_add(OUTPUT_LEASE_US),
            recover_until: now.saturating_add(RECOVERY_US),
        });
        self.phase = Phase::Starting;
        Ok(next)
    }

    pub fn remote_stop(&mut self, grant: Grant) -> Result<(), SessionError> {
        self.check(grant)?;
        self.clear();
        Ok(())
    }

    pub fn local_stop(&mut self) {
        self.set_receiving(false);
    }

    pub fn allows(&mut self, grant: Grant, now: u64) -> bool {
        self.tick(now).is_ok() && self.check(grant).is_ok() && self.phase == Phase::Casting
    }

    fn check(&self, grant: Grant) -> Result<(), SessionError> {
        if self
            .active
            .as_ref()
            .is_none_or(|active| active.grant != grant)
        {
            return Err(SessionError::Stale);
        }
        Ok(())
    }

    fn clear(&mut self) {
        self.active = None;
        self.phase = Phase::Idle;
    }
}
