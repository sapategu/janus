//! One instance per control connection; grants never come from global lookups.
use crate::{Coordinator, Grant, SessionError, Source};
use anyhow::{Result, bail, ensure};
use janus_wire::{Code, Envelope, Mode, Payload, Status};
use std::collections::VecDeque;

#[derive(Default)]
pub struct ControlPeer {
    owned: Option<Grant>,
    stopped: Option<Grant>,
    greeted: bool,
    last_sequence: u64,
    response_sequence: u64,
    cache: VecDeque<(Envelope, Envelope)>,
    window: u64,
    requests: u32,
}

fn code(error: SessionError) -> Code {
    match error {
        SessionError::Disabled => Code::Disabled,
        SessionError::Busy => Code::Busy,
        SessionError::Stale => Code::Stale,
        SessionError::Expired => Code::Expired,
        SessionError::Unsupported => Code::Unsupported,
        SessionError::Invalid => Code::Invalid,
    }
}

fn matches(grant: Grant, request: &Envelope) -> bool {
    grant.session.as_slice() == request.session_id && grant.generation == request.generation
}

impl ControlPeer {
    /// Grant currently owned by this connection, if any.
    pub fn owned(&self) -> Option<Grant> {
        self.owned
    }

    pub fn disconnect(&mut self, coordinator: &mut Coordinator) {
        if let Some(grant) = self.owned.take() {
            let _ = coordinator.remote_stop(grant);
        }
    }

    pub fn dispatch(
        &mut self,
        coordinator: &mut Coordinator,
        request: Envelope,
        now: u64,
    ) -> Result<Envelope> {
        request.validate()?;
        coordinator.tick(now)?;
        if now.saturating_sub(self.window) >= 1_000_000 {
            self.window = now;
            self.requests = 0;
        }
        self.requests += 1;
        ensure!(self.requests <= 16, "control rate limit");
        if let Some((original, response)) = self
            .cache
            .iter()
            .find(|(original, _)| original.request_id == request.request_id)
        {
            ensure!(
                *original == request,
                "request ID reused with different content"
            );
            return Ok(response.clone());
        }
        ensure!(
            request.sequence > self.last_sequence && request.reply_to == 0,
            "invalid sequence or role"
        );
        self.last_sequence = request.sequence;
        let mut generation = request.generation;
        let status = match request.payload.as_ref() {
            Some(Payload::Hello(hello)) if !self.greeted => {
                if hello.major == janus_wire::PROTOCOL_MAJOR {
                    self.greeted = true;
                    Code::Ok
                } else {
                    Code::Unsupported
                }
            }
            Some(Payload::Offer(offer)) if self.greeted && self.owned.is_none() => {
                let source = match Mode::try_from(offer.mode) {
                    Ok(Mode::Synthetic) => Source::Synthetic,
                    Ok(Mode::Video) => Source::Video,
                    Ok(Mode::AudioOnly) => Source::AudioOnly,
                    _ => bail!("unknown source"),
                };
                let session: [u8; 16] = request.session_id.as_slice().try_into()?;
                match coordinator.offer(session, source, request.sequence, now) {
                    Ok(grant) => {
                        // This service currently implements diagnostics only.
                        // Native media must wait for backend readiness, not an Offer.
                        if source == Source::Synthetic {
                            coordinator.started(grant, now)?;
                        }
                        self.owned = Some(grant);
                        generation = grant.generation;
                        Code::Ok
                    }
                    Err(error) => code(error),
                }
            }
            Some(Payload::Heartbeat(_)) if self.greeted => {
                if let Some(grant) = self.owned.filter(|grant| matches(*grant, &request)) {
                    coordinator
                        .heartbeat(grant, request.sequence, now)
                        .map(|()| Code::Ok)
                        .unwrap_or_else(code)
                } else {
                    Code::Stale
                }
            }
            Some(Payload::Stop(_)) if self.greeted => {
                if let Some(grant) = self.owned.filter(|grant| matches(*grant, &request)) {
                    match coordinator.remote_stop(grant) {
                        Ok(()) => {
                            self.stopped = Some(grant);
                            self.owned = None;
                            Code::Ok
                        }
                        Err(error) => code(error),
                    }
                } else if self.stopped.is_some_and(|grant| matches(grant, &request)) {
                    Code::Ok
                } else {
                    Code::Stale
                }
            }
            _ => Code::Invalid,
        };
        self.response_sequence = self
            .response_sequence
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("sequence exhausted"))?;
        let response = Envelope {
            request_id: self.response_sequence,
            session_id: request.session_id.clone(),
            generation,
            sequence: self.response_sequence,
            reply_to: request.request_id,
            schema_minor: 0,
            payload: Some(Payload::Status(Status {
                code: status as i32,
            })),
        };
        if self.cache.len() == 32 {
            self.cache.pop_front();
        }
        self.cache.push_back((request, response.clone()));
        Ok(response)
    }
}
