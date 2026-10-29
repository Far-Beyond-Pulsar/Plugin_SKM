//! The tracker driven by a real animation instance playing Montages.

use super::*;
use crate::runtime::anim_instance::{AnimInstance, TimeStep, UpdateMode};
use crate::runtime::clip::tests::clip_asset;
use crate::runtime::clip::CompiledClip;
use crate::runtime::montage::tests::{montage, skel};
use crate::runtime::montage::CompiledMontage;
use crate::runtime::pose::RuntimeSkeleton;

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

/// A 2 s Montage whose segment plays a 2 s clip. Montage-owned: notify `mn`
/// at 1.0 and state `ms` over 0.5..1.5. Clip-owned: notify `sn` at 0.5 and
/// state `ss` over 1.0..1.75.
fn rig() -> (Arc<RuntimeSkeleton>, Arc<CompiledMontage>) {
    let skel = Arc::new(skel());
    let base = montage(&skel, "m", "DefaultSlot", "b2", None);
    let mut m = (*base).clone();
    m.id = "montage-1".into();
    m.notifies = vec![AnimationNotify { id: "mn".into(), name: "mn".into(), time: 1.0, target: NotifyTarget::Self_, fields: vec![], schema: None }];
    m.notify_states = vec![AnimationNotifyState {
        id: "ms".into(),
        name: "ms".into(),
        start: 0.5,
        end: 1.5,
        target: NotifyTarget::Self_,
        schema: None,
        fields: vec![],
        tick_interval: None,
        disabled: false,
    }];
    let clip_seq = sequence(2.0, &[("sn", 0.5)], &[("ss", 1.0, 1.75)]);
    let clip = Arc::new(CompiledClip::compile_shared(&clip_seq, &skel));
    for t in &mut m.tracks {
        for s in &mut t.segments {
            s.clip = clip.clone();
        }
    }
    (skel, Arc::new(m))
}

fn drive(inst: &mut AnimInstance, tracker: &mut NotifyTracker, dt: f32, out: &mut Vec<NotifyOccurrence>) {
    inst.advance(dt, TimeStep::Variable, UpdateMode::Play);
    tracker.observe_report(&inst.report, out).unwrap();
}

fn count(events: &[NotifyOccurrence], id: &str, phase: NotifyPhase) -> usize {
    events.iter().filter(|e| e.id() == id && e.phase == phase).count()
}

fn names(events: &[NotifyOccurrence]) -> Vec<String> {
    events.iter().map(|e| format!("{}:{}", e.id(), e.phase.name())).collect()
}

#[test]
fn montage_and_clip_notifies_and_states_fire_exactly_once_each_with_their_sources() {
    let (skel, m) = rig();
    let mut inst = AnimInstance::new(skel);
    let mut tracker = NotifyTracker::new();
    inst.play_montage(m, 1.0, None).unwrap();
    let mut out = Vec::new();
    for _ in 0..12 {
        drive(&mut inst, &mut tracker, 0.2, &mut out);
    }
    for (id, kind) in [("mn", "montage"), ("ms", "montage"), ("sn", "animseq"), ("ss", "animseq")] {
        let owners: Vec<_> = out.iter().filter(|e| e.id() == id).map(|e| e.owner.kind()).collect();
        assert!(!owners.is_empty() && owners.iter().all(|k| *k == kind), "{id}: {owners:?}");
    }
    assert_eq!(count(&out, "mn", NotifyPhase::Notify), 1);
    assert_eq!(count(&out, "sn", NotifyPhase::Notify), 1);
    for state in ["ms", "ss"] {
        assert_eq!(count(&out, state, NotifyPhase::Begin), 1, "{state}");
        assert_eq!(count(&out, state, NotifyPhase::End), 1, "{state}");
        assert!(count(&out, state, NotifyPhase::Tick) >= 1, "{state}");
    }
    assert_eq!(tracker.live_playbacks(), 0);
    assert_eq!(tracker.active_states(), 0);
    let ms = out.iter().find(|e| e.id() == "ms").unwrap().instance;
    let ss = out.iter().find(|e| e.id() == "ss").unwrap().instance;
    assert_ne!(ms, ss, "the Montage timeline and its segment are separate playback instances");
}

#[test]
fn interrupting_a_montage_ends_its_active_states_once() {
    let (skel, m) = rig();
    let mut inst = AnimInstance::new(skel);
    let mut tracker = NotifyTracker::new();
    let first = inst.play_montage(m.clone(), 1.0, None).unwrap();
    let mut out = Vec::new();
    for _ in 0..6 {
        drive(&mut inst, &mut tracker, 0.2, &mut out);
    }
    assert_eq!(count(&out, "ms", NotifyPhase::Begin), 1);
    assert_eq!(count(&out, "ms", NotifyPhase::End), 0);
    let second = inst.play_montage(m, 1.0, None).unwrap();
    assert_ne!(first, second);
    out.clear();
    drive(&mut inst, &mut tracker, 0.1, &mut out);
    let ends: Vec<_> = out.iter().filter(|e| e.phase == NotifyPhase::End && e.key.scope == first.0).collect();
    assert_eq!(ends.len(), 2, "the Montage state and the clip state: {:?}", names(&out));
    assert!(ends.iter().all(|e| e.end_reason == Some(EndReason::Stopped)));
    for _ in 0..15 {
        drive(&mut inst, &mut tracker, 0.2, &mut out);
    }
    assert_eq!(out.iter().filter(|e| e.key.scope == first.0 && e.phase != NotifyPhase::End).count(), 0);
    assert_eq!(out.iter().filter(|e| e.key.scope == first.0 && e.phase == NotifyPhase::End).count(), 2, "ended exactly once");
    assert_eq!(tracker.live_playbacks(), 0);
}

#[test]
fn two_components_sharing_a_montage_keep_independent_clocks_and_events() {
    let (skel, m) = rig();
    let (mut a, mut b) = (AnimInstance::new(skel.clone()), AnimInstance::new(skel));
    let (mut ta, mut tb) = (NotifyTracker::new(), NotifyTracker::new());
    a.play_montage(m.clone(), 1.0, None).unwrap();
    b.play_montage(m, 2.0, None).unwrap();
    let (mut oa, mut ob) = (Vec::new(), Vec::new());
    // b runs twice as fast and finishes while a is still inside its states
    for _ in 0..5 {
        drive(&mut a, &mut ta, 0.2, &mut oa);
        drive(&mut b, &mut tb, 0.2, &mut ob);
    }
    assert_eq!(count(&ob, "ms", NotifyPhase::End), 1);
    assert_eq!(count(&oa, "ms", NotifyPhase::End), 0, "a is at 1.0 s and still inside");
    assert_eq!(ta.active_states(), 2);
    assert_eq!(tb.active_states(), 0);
    for _ in 0..6 {
        drive(&mut a, &mut ta, 0.2, &mut oa);
    }
    assert_eq!(count(&oa, "ms", NotifyPhase::Begin), 1);
    assert_eq!(count(&oa, "ms", NotifyPhase::End), 1);
}

#[test]
fn pausing_holds_states_without_events_and_resuming_continues() {
    let (skel, m) = rig();
    let mut inst = AnimInstance::new(skel);
    let mut tracker = NotifyTracker::new();
    let h = inst.play_montage(m, 1.0, None).unwrap();
    let mut out = Vec::new();
    drive(&mut inst, &mut tracker, 1.2, &mut out);
    assert_eq!(count(&out, "ms", NotifyPhase::Begin), 1);
    inst.montages.pause(h, true);
    out.clear();
    for _ in 0..5 {
        drive(&mut inst, &mut tracker, 0.5, &mut out);
    }
    assert!(out.is_empty(), "{:?}", names(&out));
    assert_eq!(tracker.active_states(), 2);
    inst.montages.pause(h, false);
    drive(&mut inst, &mut tracker, 1.0, &mut out);
    assert_eq!(count(&out, "ms", NotifyPhase::End), 1);
}

#[test]
fn scrubbing_never_reports_and_a_gameplay_seek_ends_states_as_seek() {
    let (skel, m) = rig();
    let mut inst = AnimInstance::new(skel);
    let mut tracker = NotifyTracker::new();
    let h = inst.play_montage(m, 1.0, None).unwrap();
    let mut out = Vec::new();
    drive(&mut inst, &mut tracker, 1.2, &mut out);
    out.clear();
    inst.advance(0.5, TimeStep::Variable, UpdateMode::Scrub);
    assert!(inst.report.spans.is_empty() && inst.report.montage_timelines.is_empty(), "a scrub reports no playback");
    inst.montages.seek(h, 1.8);
    tracker.end_all(EndReason::Seek, &mut out);
    assert_eq!(out.iter().filter(|e| e.end_reason == Some(EndReason::Seek)).count(), 2);
    out.clear();
    drive(&mut inst, &mut tracker, 0.1, &mut out);
    assert_eq!(count(&out, "ms", NotifyPhase::Begin), 0, "1.8 s is past the Montage state");
    assert_eq!(count(&out, "ss", NotifyPhase::Begin), 0, "and past the clip state");
}

#[test]
fn a_seek_without_telling_the_tracker_is_still_a_jump_that_ends_states() {
    let (skel, m) = rig();
    let mut inst = AnimInstance::new(skel);
    let mut tracker = NotifyTracker::new();
    let h = inst.play_montage(m, 1.0, None).unwrap();
    let mut out = Vec::new();
    drive(&mut inst, &mut tracker, 1.2, &mut out);
    out.clear();
    inst.montages.seek(h, 1.8);
    drive(&mut inst, &mut tracker, 0.1, &mut out);
    let jumps = out.iter().filter(|e| e.end_reason == Some(EndReason::Jump)).count();
    assert_eq!(jumps, 2, "{:?}", names(&out));
}

#[test]
fn reverse_playback_raises_the_same_events_in_reverse_order() {
    let (skel, m) = rig();
    let mut inst = AnimInstance::new(skel);
    let mut tracker = NotifyTracker::new();
    let h = inst.play_montage(m, -1.0, None).unwrap();
    inst.montages.seek(h, 1.9);
    let mut out = Vec::new();
    for _ in 0..10 {
        drive(&mut inst, &mut tracker, 0.2, &mut out);
    }
    let order: Vec<_> = out.iter().filter(|e| e.phase != NotifyPhase::Tick).map(|e| format!("{}:{}", e.id(), e.phase.name())).collect();
    let at = |s: &str| order.iter().position(|x| x == s).unwrap_or_else(|| panic!("{s} missing in {order:?}"));
    // from 1.9 down: the clip state (1.75..1.0) is entered first, then the Montage state (1.5..0.5)
    assert!(at("ss:begin") < at("ms:begin"));
    assert!(at("ms:begin") < at("mn:notify"), "mn at 1.0 comes after entering ms at 1.5");
    assert!(at("mn:notify") < at("sn:notify"), "sn at 0.5 is later in reverse");
    assert_eq!(count(&out, "mn", NotifyPhase::Notify), 1);
    assert_eq!(count(&out, "sn", NotifyPhase::Notify), 1);
    for state in ["ms", "ss"] {
        assert_eq!(count(&out, state, NotifyPhase::Begin), 1);
        assert_eq!(count(&out, state, NotifyPhase::End), 1);
    }
    assert_eq!(tracker.active_states(), 0);
}

#[test]
fn a_hot_reloaded_montage_ends_its_states_as_reload_and_the_new_asset_takes_over() {
    let (skel, m) = rig();
    let mut inst = AnimInstance::new(skel);
    let mut tracker = NotifyTracker::new();
    inst.play_montage(m.clone(), 1.0, None).unwrap();
    let mut out = Vec::new();
    drive(&mut inst, &mut tracker, 1.2, &mut out);
    out.clear();
    // the component swaps in the recompiled montage under the running instance
    let mut reloaded = (*m).clone();
    reloaded.notify_states[0].end = 1.2;
    let reloaded = Arc::new(reloaded);
    let line = crate::runtime::anim_instance::TimelineSpan { handle: crate::runtime::montage::MontageHandle(0), montage: reloaded.clone(), from: 0.8, to: 0.9, weight: 1.0 };
    tracker.begin_update();
    tracker.observe_montage(line.handle.0, &line.montage, line.from, line.to, line.weight, &mut out).unwrap();
    tracker.end_update(&mut out);
    let reasons: Vec<_> = out.iter().filter(|e| e.phase == NotifyPhase::End).map(|e| (e.id().to_string(), e.end_reason)).collect();
    assert!(reasons.contains(&("ms".to_string(), Some(EndReason::Reload))), "{reasons:?}");
    assert_eq!(count(&out, "ms", NotifyPhase::Begin), 1, "the reloaded asset begins its own activation");
}
