use std::collections::VecDeque;
use thiserror::Error;

const WINDOW_US: u64 = 60_000_000;
pub const MAX_SAMPLES: usize = 128;

#[derive(Clone, Copy, Debug)]
pub struct Probe {
    pub receiver_send: u64,
    pub sender_receive: u64,
    pub sender_send: u64,
    pub receiver_receive: u64,
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    local: f64,
    remote: f64,
    rtt: f64,
    at: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct ClockModel {
    /// Centered origins avoid subtracting large epoch values in the fit.
    pub receiver_origin: f64,
    pub sender_origin: f64,
    pub slope: f64,
    pub uncertainty_us: f64,
    pub drift_qualified: bool,
}

impl ClockModel {
    pub fn receiver_time(&self, sender_us: u64) -> Option<u64> {
        let time = self.receiver_origin + (sender_us as f64 - self.sender_origin) / self.slope;
        if !time.is_finite() || !(0.0..u64::MAX as f64).contains(&time) {
            return None;
        }
        Some(time.round() as u64)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ClockError {
    #[error("invalid or stale clock sample")]
    Invalid,
    #[error("clock drift exceeds the supported fit range")]
    Drift,
}

#[derive(Default)]
pub struct ClockEstimator {
    samples: VecDeque<Sample>,
}

impl ClockEstimator {
    pub fn reset(&mut self) {
        self.samples.clear();
    }
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    pub fn push(&mut self, probe: Probe) -> Result<ClockModel, ClockError> {
        let local_span = probe
            .receiver_receive
            .checked_sub(probe.receiver_send)
            .ok_or(ClockError::Invalid)?;
        let remote_span = probe
            .sender_send
            .checked_sub(probe.sender_receive)
            .ok_or(ClockError::Invalid)?;
        let rtt = local_span
            .checked_sub(remote_span)
            .ok_or(ClockError::Invalid)?;
        if rtt > 500_000
            || remote_span > 100_000
            || self
                .samples
                .back()
                .is_some_and(|sample| probe.receiver_send <= sample.at)
        {
            return Err(ClockError::Invalid);
        }
        let sample = Sample {
            local: probe.receiver_send as f64 + local_span as f64 / 2.0,
            remote: probe.sender_receive as f64 + remote_span as f64 / 2.0,
            rtt: rtt as f64,
            at: probe.receiver_send,
        };
        self.samples.push_back(sample);
        while self.samples.len() > MAX_SAMPLES
            || self
                .samples
                .front()
                .is_some_and(|old| sample.at.saturating_sub(old.at) > WINDOW_US)
        {
            self.samples.pop_front();
        }
        let model = self.fit()?;
        Ok(model)
    }

    fn fit(&self) -> Result<ClockModel, ClockError> {
        let min_rtt = self
            .samples
            .iter()
            .map(|s| s.rtt)
            .fold(f64::INFINITY, f64::min);
        let selected: Vec<_> = self
            .samples
            .iter()
            .filter(|s| s.rtt <= min_rtt + 2_000.0)
            .collect();
        let count = selected.len() as f64;
        let origin = selected[0];
        let x = selected.iter().map(|s| s.local - origin.local).sum::<f64>() / count;
        let y = selected
            .iter()
            .map(|s| s.remote - origin.remote)
            .sum::<f64>()
            / count;
        let variance = selected
            .iter()
            .map(|s| (s.local - origin.local - x).powi(2))
            .sum::<f64>();
        let span = selected.last().unwrap().at.saturating_sub(selected[0].at);
        let drift_qualified = selected.len() >= 6 && span >= 5_000_000 && variance > 0.0;
        let slope = if drift_qualified {
            selected
                .iter()
                .map(|s| (s.local - origin.local - x) * (s.remote - origin.remote - y))
                .sum::<f64>()
                / variance
        } else {
            1.0
        };
        if !slope.is_finite() || (slope - 1.0).abs() > 0.0005 {
            return Err(ClockError::Drift);
        }
        let receiver_origin = origin.local + x;
        let sender_origin = origin.remote + y;
        let residual = selected
            .iter()
            .map(|s| (s.remote - (sender_origin + slope * (s.local - receiver_origin))).abs())
            .fold(0.0, f64::max);
        // RTT/2 remains uncertainty: a low RTT does not prove link symmetry.
        let spread = self.samples.iter().map(|s| s.rtt).fold(0.0, f64::max) - min_rtt;
        Ok(ClockModel {
            receiver_origin,
            sender_origin,
            slope,
            uncertainty_us: min_rtt / 2.0 + residual + spread / 2.0,
            drift_qualified,
        })
    }
}
