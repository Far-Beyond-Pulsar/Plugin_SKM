use super::*;
use crate::runtime::clip::tests::clip_asset;

fn sequence(duration: f32, notifies: &[(&str, f32)], states: &[(&str, f32, f32)]) -> Arc<AnimSeqAsset> {
    let mut asset = clip_asset("c", duration, vec![]);
    asset.features.notifies = notifies
        .iter()
        .map(|(id, t)| AnimationNotify { id: (*id).into(), name: (*id).into(), time: *t, target: NotifyTarget::Self_, fields: vec![], schema: None })
        .collect();
    asset.features.notify_states = states
        .iter()
        .map(|(id, s, e)| AnimationNotifyState {
            id: (*id).into(),
            name: (*id).into(),
            start: *s,
            end: *e,
            target: NotifyTarget::Self_,
            schema: None,
            fields: vec![],
            tick_interval: None,
            disabled: false,
        })
        .collect();
    Arc::new(asset)
}

fn key(scope: u64) -> PlaybackKey {
    PlaybackKey { scope, origin: SpanOrigin::SequencePlayer { node: "player".into() } }
}

/// `id:phase` of every occurrence, with the end reason appended to ends.
fn names(events: &[NotifyOccurrence]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e.end_reason {
            Some(r) => format!("{}:{}:{}", e.id(), e.phase.name(), r.name()),
            None => format!("{}:{}", e.id(), e.phase.name()),
        })
        .collect()
}

/// One update of the playback `scope` from `from` to `to`.
fn step(t: &mut NotifyTracker, seq: &Arc<AnimSeqAsset>, scope: u64, from: f64, to: f64, looping: bool) -> Vec<NotifyOccurrence> {
    let mut out = Vec::new();
    t.begin_update();
    t.observe(key(scope), NotifyOwner::Sequence(seq.clone()), from, to, looping, 1.0, &mut out).unwrap();
    t.end_update(&mut out);
    out
}

#[test]
fn a_state_begins_ticks_and_ends_once_across_updates() {
    let seq = sequence(4.0, &[], &[("s", 1.0, 2.0)]);
    let mut t = NotifyTracker::new();
    assert!(step(&mut t, &seq, 1, 0.0, 0.5, false).is_empty());
    assert_eq!(names(&step(&mut t, &seq, 1, 0.5, 1.25, false)), ["s:begin", "s:tick"]);
    assert_eq!(names(&step(&mut t, &seq, 1, 1.25, 1.75, false)), ["s:tick"]);
    assert_eq!(names(&step(&mut t, &seq, 1, 1.75, 2.5, false)), ["s:tick", "s:end:completed"]);
    assert!(step(&mut t, &seq, 1, 2.5, 3.0, false).is_empty());
    assert_eq!(t.active_states(), 0);
}

#[test]
fn tick_deltas_add_up_to_the_time_spent_inside() {
    let seq = sequence(4.0, &[], &[("s", 1.0, 2.0)]);
    let mut t = NotifyTracker::new();
    let mut total = 0.0;
    for (a, b) in [(0.0, 0.9), (0.9, 1.4), (1.4, 1.6), (1.6, 3.0)] {
        total += step(&mut t, &seq, 1, a, b, false).iter().filter(|e| e.phase == NotifyPhase::Tick).map(|e| e.delta as f64).sum::<f64>();
    }
    assert!((total - 1.0).abs() < 1e-5, "{total}");
}

#[test]
fn a_high_delta_runs_begin_notify_tick_end_in_playhead_order() {
    let seq = sequence(4.0, &[("n", 1.5)], &[("s", 1.0, 2.0)]);
    let mut t = NotifyTracker::new();
    let events = step(&mut t, &seq, 1, 0.0, 3.5, false);
    assert_eq!(names(&events), ["s:begin", "n:notify", "s:tick", "s:end:completed"]);
    assert!(events.iter().all(|e| e.instance == events[0].instance));
}

#[test]
fn looping_high_delta_gives_one_pair_per_cycle_however_the_motion_is_split() {
    let seq = sequence(1.0, &[("n", 0.5)], &[("s", 0.25, 0.75)]);
    let mut t = NotifyTracker::new();
    let events = step(&mut t, &seq, 1, 0.0, 3.0, true);
    let count = |p: NotifyPhase| events.iter().filter(|e| e.phase == p).count();
    assert_eq!((count(NotifyPhase::Begin), count(NotifyPhase::End), count(NotifyPhase::Notify)), (3, 3, 3));
    let cycles: Vec<i64> = events.iter().filter(|e| e.phase == NotifyPhase::Begin).map(|e| e.cycle).collect();
    assert_eq!(cycles, [0, 1, 2]);
    assert_eq!(names(&events)[..4], ["s:begin", "n:notify", "s:tick", "s:end:completed"]);
    let mut t2 = NotifyTracker::new();
    let mut split = Vec::new();
    for (a, b) in [(0.0, 0.6), (0.6, 1.9), (1.9, 3.0)] {
        split.extend(step(&mut t2, &seq, 1, a, b, true));
    }
    let strip = |v: &[NotifyOccurrence]| names(v).into_iter().filter(|n| !n.ends_with(":tick")).collect::<Vec<_>>();
    assert_eq!(strip(&events), strip(&split));
}

#[test]
fn a_state_ending_at_the_clip_end_ends_at_the_wrap_and_begins_again_next_cycle() {
    let seq = sequence(1.0, &[], &[("s", 0.5, 1.0)]);
    let mut t = NotifyTracker::new();
    assert_eq!(names(&step(&mut t, &seq, 1, 0.0, 0.75, true)), ["s:begin", "s:tick"]);
    assert_eq!(names(&step(&mut t, &seq, 1, 0.75, 1.25, true)), ["s:tick", "s:end:completed"]);
    assert_eq!(names(&step(&mut t, &seq, 1, 1.25, 1.75, true)), ["s:begin", "s:tick"]);
}

#[test]
fn reverse_playback_mirrors_forward() {
    let seq = sequence(4.0, &[("n", 1.5)], &[("s", 1.0, 2.0)]);
    let mut t = NotifyTracker::new();
    assert!(step(&mut t, &seq, 1, 3.0, 2.5, false).is_empty());
    let events = step(&mut t, &seq, 1, 2.5, 0.5, false);
    assert_eq!(names(&events), ["s:begin", "n:notify", "s:tick", "s:end:completed"]);
    assert_eq!(events[0].position, 2.0, "entered at the state's end going backwards");
    assert_eq!(events[3].position, 1.0);
}

#[test]
fn a_reverse_loop_crosses_the_seam_backwards() {
    let seq = sequence(1.0, &[], &[("s", 0.25, 0.75)]);
    let mut t = NotifyTracker::new();
    assert!(step(&mut t, &seq, 1, 1.1, 0.9, true).is_empty());
    let events = step(&mut t, &seq, 1, 0.9, -0.5, true);
    assert_eq!(names(&events), ["s:begin", "s:tick", "s:end:completed", "s:begin", "s:tick"]);
    assert_eq!(events.iter().filter(|e| e.phase == NotifyPhase::Begin).map(|e| e.cycle).collect::<Vec<_>>(), [0, -1]);
}

#[test]
fn a_jump_ends_the_active_state_and_a_landing_inside_begins_it_again() {
    let seq = sequence(4.0, &[], &[("s", 1.0, 2.0)]);
    let mut t = NotifyTracker::new();
    step(&mut t, &seq, 1, 0.0, 1.5, false);
    // the clock moved from 1.5 to 3.0 without playing the time between
    assert_eq!(names(&step(&mut t, &seq, 1, 3.0, 3.25, false)), ["s:end:jump"]);
    assert_eq!(names(&step(&mut t, &seq, 1, 1.25, 1.5, false)), ["s:begin", "s:tick"]);
}

#[test]
fn a_playback_that_stops_contributing_ends_its_states_once() {
    let seq = sequence(4.0, &[], &[("s", 1.0, 3.0)]);
    let mut t = NotifyTracker::new();
    step(&mut t, &seq, 1, 0.0, 1.5, false);
    let mut out = Vec::new();
    t.begin_update();
    t.end_update(&mut out);
    assert_eq!(names(&out), ["s:end:stopped"]);
    out.clear();
    t.begin_update();
    t.end_update(&mut out);
    assert!(out.is_empty(), "ended once");
    assert_eq!(t.live_playbacks(), 0);
    assert_eq!(names(&step(&mut t, &seq, 1, 1.5, 2.0, false)), ["s:begin", "s:tick"], "relevant again: a fresh activation");
}

#[test]
fn a_paused_playback_stays_active_without_events() {
    let seq = sequence(4.0, &[("n", 1.5)], &[("s", 1.0, 3.0)]);
    let mut t = NotifyTracker::new();
    step(&mut t, &seq, 1, 0.0, 1.5, false);
    for _ in 0..3 {
        assert!(step(&mut t, &seq, 1, 1.5, 1.5, false).is_empty());
    }
    assert_eq!(t.active_states(), 1);
    assert_eq!(names(&step(&mut t, &seq, 1, 1.5, 3.5, false)), ["s:tick", "s:end:completed"]);
}

#[test]
fn replacing_the_asset_ends_states_as_a_reload_and_starts_a_new_instance() {
    let seq = sequence(4.0, &[], &[("s", 1.0, 3.0)]);
    let mut t = NotifyTracker::new();
    step(&mut t, &seq, 1, 0.0, 1.5, false);
    let edited = Arc::new((*seq).clone());
    let out = step(&mut t, &edited, 1, 1.5, 2.0, false);
    assert_eq!(names(&out), ["s:end:reload", "s:begin", "s:tick"]);
    assert_ne!(out[0].instance, out[1].instance);
}

#[test]
fn seek_and_teardown_end_every_state_with_their_own_reason_once() {
    let seq = sequence(4.0, &[], &[("s", 1.0, 3.0)]);
    let mut t = NotifyTracker::new();
    step(&mut t, &seq, 1, 0.0, 1.5, false);
    let mut out = Vec::new();
    t.end_all(EndReason::Seek, &mut out);
    assert_eq!(names(&out), ["s:end:seek"]);
    out.clear();
    t.end_all(EndReason::Teardown, &mut out);
    assert!(out.is_empty(), "nothing left to end");
    assert_eq!(names(&step(&mut t, &seq, 1, 2.0, 2.5, false)), ["s:begin", "s:tick"], "a clean start at the new position");
}

#[test]
fn two_playbacks_of_one_asset_are_independent() {
    let seq = sequence(4.0, &[("n", 1.0)], &[("s", 1.0, 3.0)]);
    let mut t = NotifyTracker::new();
    let mut out = Vec::new();
    t.begin_update();
    t.observe(key(1), NotifyOwner::Sequence(seq.clone()), 0.0, 2.0, false, 1.0, &mut out).unwrap();
    t.observe(key(2), NotifyOwner::Sequence(seq.clone()), 0.0, 0.5, false, 1.0, &mut out).unwrap();
    t.end_update(&mut out);
    assert_eq!(out.iter().filter(|e| e.key.scope == 1).count(), 3);
    assert!(out.iter().all(|e| e.key.scope == 1), "playback 2 has not reached anything yet");
    t.begin_update();
    t.observe(key(2), NotifyOwner::Sequence(seq), 0.5, 1.5, false, 1.0, &mut out).unwrap();
    out.clear();
    t.end_update(&mut out);
    assert_eq!(names(&out), ["s:end:stopped"]);
    assert_eq!(out[0].key.scope, 1, "only the playback that stopped ended");
    assert_eq!(t.live_playbacks(), 1);
}

#[test]
fn a_tick_interval_throttles_ticks_and_disabled_states_stay_silent() {
    let mut asset = (*sequence(10.0, &[], &[("s", 0.0, 10.0), ("off", 0.0, 10.0)])).clone();
    asset.features.notify_states[0].tick_interval = Some(1.0);
    asset.features.notify_states[1].disabled = true;
    let seq = Arc::new(asset);
    let mut t = NotifyTracker::new();
    let mut all = Vec::new();
    for i in 0..20 {
        all.extend(step(&mut t, &seq, 1, i as f64 * 0.25, (i + 1) as f64 * 0.25, false));
    }
    assert_eq!(all.iter().filter(|e| e.phase == NotifyPhase::Tick).count(), 5, "5 s at one tick per second");
    assert!(all.iter().all(|e| e.id() == "s"));
}

#[test]
fn cycle_relative_looping_spans_are_rebased_not_read_as_jumps() {
    // Pose Search reports within one cycle; a wrap must neither end nor repeat a state
    let seq = sequence(1.0, &[], &[("s", 0.8, 1.0)]);
    let mut t = NotifyTracker::new();
    assert_eq!(names(&step(&mut t, &seq, 1, 0.0, 0.9, true)), ["s:begin", "s:tick"]);
    assert_eq!(names(&step(&mut t, &seq, 1, 0.9, 1.1, true)), ["s:tick", "s:end:completed"]);
    let relative = step(&mut t, &seq, 1, 0.1, 0.9, true);
    assert_eq!(names(&relative), ["s:begin", "s:tick"]);
    assert_eq!(relative[0].cycle, 1, "continues the unbounded count, no restart");
}

#[test]
fn work_is_bounded() {
    let seq = sequence(1.0, &[], &[("s", 0.25, 0.75)]);
    let mut t = NotifyTracker::new();
    let mut out = Vec::new();
    t.begin_update();
    let result = t.observe(key(1), NotifyOwner::Sequence(seq), 0.0, 5.0e6, true, 1.0, &mut out);
    assert!(result.is_err());
    assert!(out.is_empty());
}

#[test]
fn a_fresh_playback_that_starts_inside_a_state_begins_it() {
    let seq = sequence(4.0, &[], &[("s", 0.0, 2.0)]);
    let mut t = NotifyTracker::new();
    assert_eq!(names(&step(&mut t, &seq, 1, 0.0, 0.5, false)), ["s:begin", "s:tick"]);
}

#[test]
fn a_zero_length_clip_or_asset_without_events_is_silent() {
    let empty = sequence(2.0, &[], &[]);
    let mut t = NotifyTracker::new();
    assert!(step(&mut t, &empty, 1, 0.0, 5.0, true).is_empty());
    assert_eq!(t.live_playbacks(), 1, "still tracked so a reload on the same key is noticed");
}
