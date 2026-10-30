//! Notify playback instances: which notifies and notify states a component's
//! playbacks raise, exactly once.
//!
//! A *playback* is one clock that plays one asset's timeline: a Sequence Player
//! node, one contributing Blend Space sample, a Pose Search cursor, one segment
//! of one Montage instance, or a Montage instance's own timeline. The tracker
//! keeps one record per playback (keyed by the graph/Montage instance that owns
//! it and where in that graph it sits), so two components, two Montage
//! instances, or two nodes playing the same clip never share notify state.
//!
//! The tracker is pure and headless. It never reads a clock; it is fed the
//! spans the animation instance reports (`from -> to` in unbounded seconds on
//! the asset's timeline, `from == to` meaning "live, did not move") and turns
//! them into [`NotifyOccurrence`]s:
//!
//! * a **notify** fires when the playhead passes it (departure excluded,
//!   arrival included), in either direction, across any number of loops;
//! * a **notify state** is entered when the playhead is inside its range and
//!   left when it is not, giving [`NotifyPhase::Begin`], one
//!   [`NotifyPhase::Tick`] per update while inside (or per `tick_interval`),
//!   and [`NotifyPhase::End`] with a reason. Each activation begins once and
//!   ends once, however the playback stops.
//!
//! What stops a state, with the reason it reports:
//!
//! | situation | reason |
//! |---|---|
//! | the playhead leaves the range (forward, reverse, wrap, big delta) | `Completed` |
//! | the clock jumps (section jump/link, seek, Pose Search switch) | `Jump` |
//! | an explicit seek/restart by the owner ([`NotifyTracker::end_all`]) | `Seek` |
//! | the playback stops contributing (blend weight zero, Montage ended/interrupted/stopped, node left the graph) | `Stopped` |
//! | the asset behind the playback was replaced (hot reload) | `Reload` |
//! | the owner goes away ([`NotifyTracker::end_all`]) | `Teardown` |
//!
//! Pausing is simply no updates: nothing begins, ticks or ends. Scrubbing is
//! never reported to the tracker (the animation instance reports no spans in
//! `UpdateMode::Scrub`), so preview scrubbing cannot emit gameplay events.
//!
//! Ordering inside one span is by distance travelled along the playhead's
//! path, then Tick < End < Begin < Notify, then cycle in playback direction,
//! then the asset's own order. The work of one span is bounded:
//! more than [`MAX_STATE_TRANSITIONS`] state cycles (or a million notify
//! crossings) is an error and nothing from that span is reported.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::assets::AnimSeqAsset;
use crate::notifies::{candidate_cycles, AnimationNotify, NotifyField, NotifyIndex, NotifyTarget, PlaybackInterval};
use crate::notify_schema::AnimationNotifyState;

use super::graph::{PlaybackSpan, SpanOrigin};
use super::montage::CompiledMontage;

/// Upper bound on (state, loop cycle) pairs examined for one span.
pub const MAX_STATE_TRANSITIONS: usize = 1_000_000;

/// Which asset owns the notifies a playback raises.
#[derive(Clone, Debug)]
pub enum NotifyOwner {
    Sequence(Arc<AnimSeqAsset>),
    Montage(Arc<CompiledMontage>),
}

impl NotifyOwner {
    /// Descriptor namespace and `source_kind` of occurrences from this owner.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Sequence(_) => "animseq",
            Self::Montage(_) => "montage",
        }
    }

    pub fn id(&self) -> String {
        match self {
            Self::Sequence(s) => s.id.to_string(),
            Self::Montage(m) => m.id.clone(),
        }
    }

    /// The very same loaded asset.
    pub fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Sequence(a), Self::Sequence(b)) => Arc::ptr_eq(a, b),
            (Self::Montage(a), Self::Montage(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    pub fn notifies(&self) -> &[AnimationNotify] {
        match self {
            Self::Sequence(s) => &s.features.notifies,
            Self::Montage(m) => &m.notifies,
        }
    }

    pub fn states(&self) -> &[AnimationNotifyState] {
        match self {
            Self::Sequence(s) => &s.features.notify_states,
            Self::Montage(m) => &m.notify_states,
        }
    }

    /// Length of the timeline the notifies are authored on.
    pub fn length(&self) -> f32 {
        match self {
            Self::Sequence(s) => s.clip.duration,
            Self::Montage(m) => m.length,
        }
    }

    fn has_events(&self) -> bool {
        !self.notifies().is_empty() || !self.states().is_empty()
    }
}

/// Identity of one playback within a component.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlaybackKey {
    /// Graph instance or Montage instance that owns the playing node.
    pub scope: u64,
    pub origin: SpanOrigin,
}

impl PlaybackKey {
    /// A Montage instance's own timeline (its authored notifies), not a segment's clip.
    pub fn montage_timeline(handle: u64, montage: &str) -> Self {
        Self { scope: handle, origin: SpanOrigin::Montage { montage: montage.to_string(), segment: String::new() } }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyPhase {
    /// An instant notify.
    Notify,
    /// A notify state was entered.
    Begin,
    /// A notify state is being played through.
    Tick,
    /// A notify state was left.
    End,
}

impl NotifyPhase {
    pub fn name(self) -> &'static str {
        match self {
            Self::Notify => "notify",
            Self::Begin => "begin",
            Self::Tick => "tick",
            Self::End => "end",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    Completed,
    Jump,
    Seek,
    Stopped,
    Reload,
    Teardown,
}

impl EndReason {
    pub fn name(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Jump => "jump",
            Self::Seek => "seek",
            Self::Stopped => "stopped",
            Self::Reload => "reload",
            Self::Teardown => "teardown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyItem {
    Notify(usize),
    State(usize),
}

/// One thing the tracker decided to report.
#[derive(Clone, Debug)]
pub struct NotifyOccurrence {
    pub owner: NotifyOwner,
    pub item: NotifyItem,
    pub phase: NotifyPhase,
    pub end_reason: Option<EndReason>,
    pub key: PlaybackKey,
    /// Serial of the playback instance (increasing per tracker, never reused).
    pub instance: u64,
    /// Unbounded position on the asset's timeline when it happened.
    pub position: f64,
    /// Loop cycle of `position`.
    pub cycle: i64,
    /// Ticks: seconds of the asset's timeline played inside the state since the last tick.
    pub delta: f32,
    /// Contribution weight of the playback when it was reported.
    pub weight: f32,
}

impl NotifyOccurrence {
    fn notify(&self) -> Option<&AnimationNotify> {
        match self.item {
            NotifyItem::Notify(i) => self.owner.notifies().get(i),
            NotifyItem::State(_) => None,
        }
    }

    fn state(&self) -> Option<&AnimationNotifyState> {
        match self.item {
            NotifyItem::State(i) => self.owner.states().get(i),
            NotifyItem::Notify(_) => None,
        }
    }

    pub fn id(&self) -> &str {
        self.notify().map(|n| n.id.as_str()).or_else(|| self.state().map(|s| s.id.as_str())).unwrap_or("")
    }

    pub fn name(&self) -> &str {
        self.notify().map(|n| n.name.as_str()).or_else(|| self.state().map(|s| s.name.as_str())).unwrap_or("")
    }

    pub fn target(&self) -> &NotifyTarget {
        static SELF: NotifyTarget = NotifyTarget::Self_;
        self.notify().map(|n| &n.target).or_else(|| self.state().map(|s| &s.target)).unwrap_or(&SELF)
    }

    pub fn fields(&self) -> &[NotifyField] {
        self.notify().map(|n| n.fields.as_slice()).or_else(|| self.state().map(|s| s.fields.as_slice())).unwrap_or(&[])
    }

    /// Authored time: a notify's time, a state's start (begin) or end (end/tick).
    pub fn asset_time(&self) -> f32 {
        match (self.notify(), self.state()) {
            (Some(n), _) => n.time,
            (_, Some(s)) if self.phase == NotifyPhase::Begin => s.start,
            (_, Some(s)) => s.end,
            _ => 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Active {
    state: usize,
    cycle: i64,
    since_tick: f32,
}

#[derive(Debug)]
struct Playback {
    instance: u64,
    owner: NotifyOwner,
    /// Where the playhead was after the latest span (rebased, unbounded).
    last_to: f64,
    last_cycle: i64,
    active: Vec<Active>,
    seen: bool,
}

struct CachedIndex {
    owner: NotifyOwner,
    index: NotifyIndex,
}

/// Per-component record of every live playback and its active notify states.
#[derive(Default)]
pub struct NotifyTracker {
    next_instance: u64,
    playbacks: BTreeMap<PlaybackKey, Playback>,
    indexes: HashMap<(&'static str, String), CachedIndex>,
}

impl std::fmt::Debug for NotifyTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotifyTracker").field("playbacks", &self.playbacks.len()).finish()
    }
}

struct Pending {
    progress: f64,
    rank: u8,
    cycle_order: i64,
    index: usize,
    occurrence: NotifyOccurrence,
}

impl NotifyTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of live playbacks (with or without notify content).
    pub fn live_playbacks(&self) -> usize {
        self.playbacks.len()
    }

    /// Notify states currently active across all playbacks.
    pub fn active_states(&self) -> usize {
        self.playbacks.values().map(|p| p.active.len()).sum()
    }

    /// Forget everything without reporting (a component starting over before
    /// anything was delivered).
    pub fn reset(&mut self) {
        self.playbacks.clear();
        self.indexes.clear();
    }

    /// Start of one animation advance: playbacks must be seen again to stay live.
    pub fn begin_update(&mut self) {
        for playback in self.playbacks.values_mut() {
            playback.seen = false;
        }
    }

    /// Feed a span reported for a sequence clip (graph player, Blend Space sample,
    /// Pose Search cursor or Montage segment).
    pub fn observe_clip(&mut self, span: &PlaybackSpan, out: &mut Vec<NotifyOccurrence>) -> Result<(), String> {
        let Some(sequence) = &span.clip.source else { return Ok(()) };
        let owner = NotifyOwner::Sequence(sequence.clone());
        let key = PlaybackKey { scope: span.scope, origin: span.origin.clone() };
        self.observe_with_seam(key, owner, span.from, span.to, span.looping, span.weight, span.seam, out)
    }

    /// Feed the span of a Montage instance's own timeline.
    pub fn observe_montage(
        &mut self,
        handle: u64,
        montage: &Arc<CompiledMontage>,
        from: f64,
        to: f64,
        weight: f32,
        out: &mut Vec<NotifyOccurrence>,
    ) -> Result<(), String> {
        let key = PlaybackKey::montage_timeline(handle, &montage.id);
        self.observe(key, NotifyOwner::Montage(montage.clone()), from, to, false, weight, out)
    }

    /// Feed everything one advance reported (graph and Montage segment spans, the
    /// Montage timelines) and close the update: playbacks that stopped contributing
    /// end their states, and so do those of Montage instances that finished in it.
    /// Every span is processed even if one fails; the first error is returned.
    pub fn observe_report(&mut self, report: &super::anim_instance::UpdateReport, out: &mut Vec<NotifyOccurrence>) -> Result<(), String> {
        self.begin_update();
        let mut first_error = None;
        for span in &report.spans {
            if let Err(e) = self.observe_clip(span, out) {
                first_error.get_or_insert(e);
            }
        }
        for line in &report.montage_timelines {
            if let Err(e) = self.observe_montage(line.handle.0, &line.montage, line.from, line.to, line.weight, out) {
                first_error.get_or_insert(e);
            }
        }
        for handle in &report.finished_montages {
            self.end_montage(handle.0, out);
        }
        self.end_update(out);
        first_error.map_or(Ok(()), Err)
    }

    /// A Montage instance ended: its segment and timeline playbacks end now.
    pub fn end_montage(&mut self, handle: u64, out: &mut Vec<NotifyOccurrence>) {
        let gone: Vec<PlaybackKey> =
            self.playbacks.keys().filter(|k| k.scope == handle && matches!(k.origin, SpanOrigin::Montage { .. })).cloned().collect();
        for key in gone {
            if let Some(playback) = self.playbacks.remove(&key) {
                end_states(&key, &playback, EndReason::Stopped, playback.last_to, out);
            }
        }
    }

    /// End of one advance: playbacks that were not seen stopped contributing, so
    /// their active states end (reason `Stopped`).
    pub fn end_update(&mut self, out: &mut Vec<NotifyOccurrence>) {
        let gone: Vec<PlaybackKey> = self.playbacks.iter().filter(|(_, p)| !p.seen).map(|(k, _)| k.clone()).collect();
        for key in gone {
            if let Some(playback) = self.playbacks.remove(&key) {
                end_states(&key, &playback, EndReason::Stopped, playback.last_to, out);
            }
        }
    }

    /// End every active state now and forget the playheads (the owner seeked,
    /// restarted, reloaded or is going away). Playbacks stay registered, so the
    /// next span starts a clean activation under the same instance.
    pub fn end_all(&mut self, reason: EndReason, out: &mut Vec<NotifyOccurrence>) {
        for (key, playback) in &mut self.playbacks {
            end_states(key, playback, reason, playback.last_to, out);
            playback.active.clear();
            // the next span has no continuity with the old playhead
            playback.last_to = f64::NAN;
        }
    }

    fn index_for<'a>(indexes: &'a mut HashMap<(&'static str, String), CachedIndex>, owner: &NotifyOwner) -> Result<&'a NotifyIndex, String> {
        let id = (owner.kind(), owner.id());
        let stale = indexes.get(&id).is_none_or(|c| !c.owner.same(owner));
        if stale {
            let duration = owner.length();
            for state in owner.states() {
                state.validate(duration)?;
            }
            let index = NotifyIndex::new(owner.notifies(), duration)?;
            indexes.insert(id.clone(), CachedIndex { owner: owner.clone(), index });
        }
        Ok(&indexes[&id].index)
    }

    #[allow(clippy::too_many_arguments)]
    fn observe(
        &mut self,
        key: PlaybackKey,
        owner: NotifyOwner,
        from: f64,
        to: f64,
        looping: bool,
        weight: f32,
        out: &mut Vec<NotifyOccurrence>,
    ) -> Result<(), String> {
        self.observe_with_seam(key, owner, from, to, looping, weight, None, out)
    }

    #[allow(clippy::too_many_arguments)]
    fn observe_with_seam(
        &mut self,
        key: PlaybackKey,
        owner: NotifyOwner,
        from: f64,
        to: f64,
        looping: bool,
        weight: f32,
        seam: Option<(f64, f64)>,
        out: &mut Vec<NotifyOccurrence>,
    ) -> Result<(), String> {
        if !from.is_finite() || !to.is_finite() {
            return Err("Playback times must be finite".into());
        }
        // liveness is recorded even for assets with nothing to report, so an
        // owner swap on the same key is still seen
        let Self { next_instance, playbacks, indexes } = self;
        let duration = owner.length() as f64;

        // an asset replaced under a running playback: end its states and start over
        if let Some(existing) = playbacks.get(&key) {
            if !existing.owner.same(&owner) {
                let reason = if existing.owner.kind() == owner.kind() && existing.owner.id() == owner.id() { EndReason::Reload } else { EndReason::Stopped };
                end_states(&key, existing, reason, existing.last_to, out);
                playbacks.remove(&key);
            }
        }
        if !owner.has_events() {
            // nothing to track; keep the entry only so a later replacement is noticed
            let playback = playbacks.entry(key).or_insert_with(|| {
                *next_instance += 1;
                Playback { instance: *next_instance, owner: owner.clone(), last_to: to, last_cycle: 0, active: Vec::new(), seen: true }
            });
            playback.seen = true;
            playback.last_to = to;
            return Ok(());
        }
        let index = Self::index_for(indexes, &owner)?;
        let fresh = !playbacks.contains_key(&key);
        let playback = playbacks.entry(key.clone()).or_insert_with(|| {
            *next_instance += 1;
            Playback { instance: *next_instance, owner: owner.clone(), last_to: f64::NAN, last_cycle: 0, active: Vec::new(), seen: true }
        });
        playback.seen = true;

        // continuity with the previous span; looping clocks may restart their
        // cycle count (Pose Search reports within one cycle), which is rebased
        let (mut from, mut to) = (from, to);
        if !looping {
            from = from.clamp(0.0, duration);
            to = to.clamp(0.0, duration);
        }
        let mut jumped = false;
        if !fresh && playback.last_to.is_finite() {
            let gap = from - playback.last_to;
            let eps = 1e-6 * duration.max(1.0);
            if looping && duration > 0.0 {
                let cycles = (gap / duration).round();
                if (gap - cycles * duration).abs() <= eps {
                    from -= cycles * duration;
                    to -= cycles * duration;
                } else {
                    jumped = true;
                }
            } else if let Some((start, end)) = seam.filter(|(start, end)| (playback.last_to - end).abs() <= eps && (from - start).abs() <= eps && end > start) {
                // the next cycle of a looping Montage segment: the run goes on across the seam
                let _ = (start, end);
            } else if gap.abs() > eps {
                jumped = true;
            }
        }
        if jumped {
            end_states(&key, playback, EndReason::Jump, from, out);
            playback.active.clear();
        }

        let forward = to >= from;
        let travelled = |p: f64| if forward { p - from } else { from - p };
        let mut pending: Vec<Pending> = Vec::new();
        let occurrence = |playback: &Playback, item, phase, end_reason, position: f64, cycle, delta| NotifyOccurrence {
            owner: playback.owner.clone(),
            item,
            phase,
            end_reason,
            key: key.clone(),
            instance: playback.instance,
            position,
            cycle,
            delta,
            weight,
        };

        // instant notifies
        for hit in index.collect(PlaybackInterval::Advance { from, to, looping })? {
            pending.push(Pending {
                progress: travelled(hit.playback_time),
                rank: 3,
                cycle_order: if forward { hit.cycle } else { -hit.cycle },
                index: hit.index,
                occurrence: occurrence(playback, NotifyItem::Notify(hit.index), NotifyPhase::Notify, None, hit.playback_time, hit.cycle, 0.0),
            });
        }

        // notify states
        let states = playback.owner.states();
        if !states.is_empty() && duration > 0.0 {
            let (lo, hi) = if forward { (from, to) } else { (to, from) };
            let (first, last) = if looping { candidate_cycles(lo, hi, duration)? } else { (0, 0) };
            let cycles = (last - first + 1) as usize;
            if cycles.saturating_mul(states.len()) > MAX_STATE_TRANSITIONS {
                return Err("Notify state query exceeds the work limit; split the playback interval".into());
            }
            let inside = |a: f64, b: f64, p: f64| if forward { a <= p && p < b } else { a < p && p <= b };
            let mut still_active: Vec<Active> = Vec::new();
            let mut touched: Vec<(usize, i64)> = Vec::new();
            for (index, state) in states.iter().enumerate() {
                if state.disabled {
                    continue;
                }
                for cycle in first..=last {
                    let base = cycle as f64 * duration;
                    let (a, b) = (base + state.start as f64, base + state.end as f64);
                    let visits = if forward { lo < b && a <= hi } else { hi > a && lo <= b };
                    let was = playback.active.iter().find(|x| x.state == index && x.cycle == cycle).copied();
                    if !visits {
                        continue;
                    }
                    touched.push((index, cycle));
                    let (entry, exit) = if forward { (lo.max(a), b) } else { (hi.min(b), a) };
                    let overlap = (hi.min(b) - lo.max(a)).max(0.0);
                    let stays = inside(a, b, to);
                    let cycle_order = if forward { cycle } else { -cycle };
                    if was.is_none() {
                        pending.push(Pending {
                            progress: travelled(entry),
                            rank: 2,
                            cycle_order,
                            index,
                            occurrence: occurrence(playback, NotifyItem::State(index), NotifyPhase::Begin, None, entry, cycle, 0.0),
                        });
                    }
                    let mut act = was.unwrap_or(Active { state: index, cycle, since_tick: 0.0 });
                    act.since_tick += overlap as f32;
                    let due = overlap > 0.0 && state.tick_interval.is_none_or(|interval| act.since_tick >= interval);
                    if due {
                        let at = if stays { to } else { exit };
                        pending.push(Pending {
                            progress: travelled(at),
                            rank: 0,
                            cycle_order,
                            index,
                            occurrence: occurrence(playback, NotifyItem::State(index), NotifyPhase::Tick, None, at, cycle, act.since_tick),
                        });
                        act.since_tick = 0.0;
                    }
                    if stays {
                        still_active.push(act);
                    } else {
                        pending.push(Pending {
                            progress: travelled(exit),
                            rank: 1,
                            cycle_order,
                            index,
                            occurrence: occurrence(playback, NotifyItem::State(index), NotifyPhase::End, Some(EndReason::Completed), exit, cycle, 0.0),
                        });
                    }
                }
            }
            // active states the playhead no longer reaches (a direction change at
            // a boundary) end where they stood
            for act in &playback.active {
                if !touched.contains(&(act.state, act.cycle)) {
                    pending.push(Pending {
                        progress: 0.0,
                        rank: 1,
                        cycle_order: if forward { act.cycle } else { -act.cycle },
                        index: act.state,
                        occurrence: occurrence(playback, NotifyItem::State(act.state), NotifyPhase::End, Some(EndReason::Completed), from, act.cycle, 0.0),
                    });
                }
            }
            still_active.sort_by_key(|a| (a.cycle, a.state));
            playback.active = still_active;
        }
        playback.last_to = to;
        playback.last_cycle = if duration > 0.0 { (to / duration).floor() as i64 } else { 0 };

        pending.sort_by(|a, b| {
            a.progress
                .total_cmp(&b.progress)
                .then(a.rank.cmp(&b.rank))
                .then(a.cycle_order.cmp(&b.cycle_order))
                .then(a.index.cmp(&b.index))
        });
        out.extend(pending.into_iter().map(|p| p.occurrence));
        Ok(())
    }
}

fn end_states(key: &PlaybackKey, playback: &Playback, reason: EndReason, position: f64, out: &mut Vec<NotifyOccurrence>) {
    for act in &playback.active {
        out.push(NotifyOccurrence {
            owner: playback.owner.clone(),
            item: NotifyItem::State(act.state),
            phase: NotifyPhase::End,
            end_reason: Some(reason),
            key: key.clone(),
            instance: playback.instance,
            position,
            cycle: act.cycle,
            delta: 0.0,
            weight: 0.0,
        });
    }
}

#[cfg(test)]
#[path = "playback_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "playback_montage_tests.rs"]
mod montage_tests;
