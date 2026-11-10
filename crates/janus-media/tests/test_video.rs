use bytes::Bytes;
use janus_media::video::*;
fn frame(group: u64, idr: bool, deadline: u64) -> Frame {
    Frame {
        generation: 1,
        group,
        object: if idr { 0 } else { 1 },
        idr,
        deadline_us: deadline,
        payload: Bytes::from_static(b"access-unit"),
    }
}
#[test]
fn expired_dependency_chain_requires_clean_idr() {
    let mut q = VideoQueue::new(1);
    assert_eq!(q.push(frame(1, true, 100), 0), Admission::Accepted);
    assert_eq!(q.push(frame(1, false, 200), 1), Admission::Accepted);
    assert!(q.pop(100).is_none());
    assert_eq!(q.queued_bytes(), 0);
    let mut next = frame(1, false, 300);
    next.object = 2;
    assert_eq!(q.push(next, 101), Admission::NeedKeyframe);
    assert_eq!(q.push(frame(2, true, 300), 102), Admission::Accepted);
}
#[test]
fn overload_drops_chain_not_arbitrary_predictive_frames() {
    let mut q = VideoQueue::new(1);
    q.push(frame(1, true, 1000), 0);
    q.push(frame(1, false, 1000), 1);
    let mut next = frame(1, false, 1000);
    next.object = 2;
    assert_eq!(q.push(next, 2), Admission::NeedKeyframe);
    assert_eq!(q.queued_frames(), 0);
    assert!(q.needs_keyframe());
}
#[test]
fn generation_reset_prevents_old_callbacks_and_staging_is_time_bounded() {
    let mut q = VideoQueue::new(1);
    q.push(frame(1, true, 100_000), 0);
    assert!(q.pop(MAX_STAGING_US).is_none());
    q.reset(2);
    assert_eq!(q.push(frame(1, true, 100_000), 40_001), Admission::Stale);
    assert_eq!(q.queued_bytes(), 0);
}

#[test]
fn missing_objects_and_replayed_idrs_cannot_reopen_old_chains() {
    let mut q = VideoQueue::new(1);
    q.push(frame(3, true, 1000), 0);
    assert!(q.pop(1).is_some());
    let mut skipped = frame(3, false, 1000);
    skipped.object = 2;
    assert_eq!(q.push(skipped, 2), Admission::NeedKeyframe);
    assert_eq!(q.push(frame(3, true, 1000), 3), Admission::Stale);
    assert_eq!(q.push(frame(2, true, 1000), 3), Admission::Stale);
    assert_eq!(q.push(frame(4, true, 1000), 4), Admission::Accepted);
}
