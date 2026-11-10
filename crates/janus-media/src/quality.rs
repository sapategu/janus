use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Profile {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u64,
}

pub const LADDER: [Profile; 4] = [
    Profile {
        width: 1920,
        height: 1080,
        fps: 60,
        bitrate: 8_000_000,
    },
    Profile {
        width: 1280,
        height: 720,
        fps: 60,
        bitrate: 5_000_000,
    },
    Profile {
        width: 1280,
        height: 720,
        fps: 30,
        bitrate: 2_500_000,
    },
    Profile {
        width: 960,
        height: 540,
        fps: 30,
        bitrate: 1_400_000,
    },
];

#[derive(Clone, Copy, Debug)]
pub struct Feedback {
    pub useful_bps: u64,
    pub queue_age_us: u64,
    pub late_fraction: f64,
    pub audio_at_risk: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Hold,
    Change(Profile),
    BelowMinimum,
}

#[derive(Default)]
pub struct Adaptation {
    rung: usize,
    pressure: u8,
    healthy_since: Option<u64>,
    last_change: Option<u64>,
    last_sample: Option<u64>,
}

impl Adaptation {
    pub fn profile(&self) -> Profile {
        LADDER[self.rung]
    }
    /// Call at most once per 200 ms window. Stale/repeated samples cannot drive adaptation.
    pub fn update(&mut self, now_us: u64, feedback: Feedback) -> Decision {
        if self
            .last_sample
            .is_some_and(|last| now_us < last.saturating_add(200_000))
        {
            return Decision::Hold;
        }
        self.last_sample = Some(now_us);
        let pressure = !feedback.late_fraction.is_finite()
            || feedback.late_fraction > 0.05
            || feedback.queue_age_us > 40_000
            || feedback.useful_bps < self.profile().bitrate.saturating_mul(11) / 10;
        if pressure || feedback.audio_at_risk {
            self.healthy_since = None;
            self.pressure = self.pressure.saturating_add(1);
            let held = self
                .last_change
                .is_none_or(|last| now_us.saturating_sub(last) >= 2_000_000);
            if feedback.audio_at_risk || (self.pressure >= 2 && held) {
                self.pressure = 0;
                if self.rung == LADDER.len() - 1 {
                    return Decision::BelowMinimum;
                }
                self.rung += 1;
                self.last_change = Some(now_us);
                return Decision::Change(self.profile());
            }
        } else {
            self.pressure = 0;
            if self.rung > 0
                && feedback.useful_bps >= LADDER[self.rung - 1].bitrate.saturating_mul(13) / 10
            {
                let since = *self.healthy_since.get_or_insert(now_us);
                if now_us.saturating_sub(since) >= 5_000_000 {
                    self.rung -= 1;
                    self.last_change = Some(now_us);
                    self.healthy_since = None;
                    return Decision::Change(self.profile());
                }
            } else {
                self.healthy_since = None;
            }
        }
        Decision::Hold
    }
}
