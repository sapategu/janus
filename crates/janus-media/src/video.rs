use bytes::Bytes;
use std::collections::VecDeque;

pub const MAX_ACCESS_UNIT: usize = 2 * 1024 * 1024;
pub const MAX_ENCODED_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_STAGING_FRAMES: usize = 2;
pub const MAX_STAGING_US: u64 = 40_000;

#[derive(Debug)]
pub struct Frame {
    pub generation: u64,
    pub group: u64,
    pub object: u64,
    pub idr: bool,
    /// Receiver-monotonic deadline, already mapped from capture time.
    pub deadline_us: u64,
    pub payload: Bytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Accepted,
    Stale,
    NeedKeyframe,
    TooLarge,
}

pub struct VideoQueue {
    generation: u64,
    group: Option<u64>,
    highest_seen: Option<(u64, u64)>,
    expected_object: u64,
    queue: VecDeque<(Frame, u64)>,
    bytes: usize,
    need_keyframe: bool,
}

impl VideoQueue {
    pub fn new(generation: u64) -> Self {
        Self {
            generation,
            group: None,
            highest_seen: None,
            expected_object: 0,
            queue: VecDeque::with_capacity(MAX_STAGING_FRAMES),
            bytes: 0,
            need_keyframe: true,
        }
    }
    pub fn queued_bytes(&self) -> usize {
        self.bytes
    }
    pub fn queued_frames(&self) -> usize {
        self.queue.len()
    }
    pub fn needs_keyframe(&self) -> bool {
        self.need_keyframe
    }

    pub fn reset(&mut self, generation: u64) {
        self.generation = generation;
        self.highest_seen = None;
        self.invalidate();
    }

    pub fn push(&mut self, frame: Frame, now: u64) -> Admission {
        if frame.generation != self.generation {
            return Admission::Stale;
        }
        let location = (frame.group, frame.object);
        if self.highest_seen.is_some_and(|highest| location <= highest) {
            return Admission::Stale;
        }
        self.highest_seen = Some(location);
        self.expire(now);
        if frame.deadline_us <= now {
            if self.group == Some(frame.group) {
                self.invalidate();
            }
            return Admission::Stale;
        }
        if frame.payload.is_empty() || frame.payload.len() > MAX_ACCESS_UNIT {
            self.invalidate();
            return Admission::TooLarge;
        }
        if self.queue.len() >= MAX_STAGING_FRAMES
            || self.bytes + frame.payload.len() > MAX_ENCODED_BYTES
        {
            self.invalidate();
        }
        if frame.idr {
            if frame.object != 0 {
                self.invalidate();
                return Admission::NeedKeyframe;
            }
            self.invalidate();
            self.need_keyframe = false;
            self.group = Some(frame.group);
        } else if self.need_keyframe
            || self.group != Some(frame.group)
            || frame.object != self.expected_object
        {
            self.invalidate();
            return Admission::NeedKeyframe;
        }
        self.expected_object = frame.object.saturating_add(1);
        self.bytes += frame.payload.len();
        self.queue.push_back((frame, now));
        Admission::Accepted
    }

    pub fn pop(&mut self, now: u64) -> Option<Frame> {
        self.expire(now);
        let (frame, _) = self.queue.pop_front()?;
        self.bytes -= frame.payload.len();
        Some(frame)
    }

    fn expire(&mut self, now: u64) {
        if self.queue.front().is_some_and(|(frame, inserted)| {
            frame.deadline_us <= now || now.saturating_sub(*inserted) >= MAX_STAGING_US
        }) {
            self.invalidate();
        }
    }

    fn invalidate(&mut self) {
        self.queue.clear();
        self.bytes = 0;
        self.group = None;
        self.need_keyframe = true;
    }
}
