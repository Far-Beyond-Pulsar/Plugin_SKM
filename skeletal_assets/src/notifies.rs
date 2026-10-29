//! named timeline events and playback interval queries.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnimationNotify {
    pub id: String,
    pub name: String,
    pub time: f32,
    #[serde(default, skip_serializing_if = "NotifyTarget::is_self")]
    pub target: NotifyTarget,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<NotifyField>,
    /// Id of the [`crate::notify_schema::NotifyPayloadSchema`] this notify's fields follow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

/// reusable assets name a class variable; they never store a live Entity handle.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "variable", rename_all = "snake_case")]
pub enum NotifyTarget {
    #[default]
    #[serde(rename = "self")]
    Self_,
    ClassVariable(String),
}

impl NotifyTarget {
    pub fn is_self(&self) -> bool {
        matches!(self, Self::Self_)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NotifyField {
    pub name: String,
    #[serde(flatten)]
    pub value: NotifyValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum NotifyValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
}

pub(crate) const RESERVED_NOTIFY_FIELDS: &[&str] = &[
    "entity",
    "source",
    "sequence_id",
    "notify_id",
    "notify_name",
    "clip_time",
    "playback_time",
    "cycle",
    "generation",
    "target",
    "class",
    "exec",
    "exec_out",
    "Body",
];

pub(crate) fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl AnimationNotify {
    pub fn validate(&self, duration: f32) -> Result<(), String> {
        if !duration.is_finite() || duration < 0.0 {
            return Err("Clip duration must be finite and nonnegative".into());
        }
        validate_notify(self, duration)
    }
    pub fn new(name: String, time: f32) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            time,
            target: NotifyTarget::Self_,
            fields: Vec::new(),
            schema: None,
        }
    }
}

/// unwrapped seconds preserve both direction and the number of crossed loops.
/// seeking and pausing are explicit, so scrubbing never dispatches events.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlaybackInterval {
    Advance { from: f64, to: f64, looping: bool },
    Seek,
    Paused,
}

#[derive(Clone, Copy, Debug)]
pub struct CrossedNotify<'a> {
    pub notify: &'a AnimationNotify,
    pub playback_time: f64,
    pub cycle: i64,
}

const MAX_NOTIFY_CROSSINGS: usize = 1_000_000;
const CROSSING_LIMIT_ERROR: &str =
    "Notify query exceeds one million crossings; split the playback interval";

/// Both sides of a wrap use the *same multiplication*. Adding duration to
/// the preceding base can round differently. Clamp interior times to these
/// boundaries as well: rounding must not put an interior notify past its end.
fn occurrence_time(cycle: i64, duration: f64, offset: f64) -> f64 {
    let start = cycle as f64 * duration;
    let end = (cycle + 1) as f64 * duration;
    if offset == duration {
        end
    } else {
        (start + offset).clamp(start, end)
    }
}

/// Division at a rounded arrival can land in a neighboring cycle. Include
/// that neighbor on both sides, plus the preceding cycle's clip-end event;
/// inclusion is decided only from occurrence_time, never inverse arithmetic.
fn candidate_cycles(from: f64, to: f64, duration: f64) -> Result<(i64, i64), String> {
    let first = (from.min(to) / duration).floor();
    let last = (from.max(to) / duration).floor();
    // f64 no longer distinguishes adjacent cycles past this range.
    if first.abs().max(last.abs()) > (1u64 << 52) as f64 {
        return Err("Playback interval is too large".into());
    }
    Ok((first as i64 - 2, last as i64 + 1))
}

/// collects crossings in playback order, excluding departure and including arrival.
/// at a wrap, the departing cycle's end precedes the arriving cycle's zero;
/// reverse playback reverses that order. equal-time notifies retain asset order.
/// zero-duration clips and seek/paused intervals produce no crossings.
/// queries above one million crossings return an error; callers can split the interval.
/// the caller chooses whether these events should be dispatched.
pub fn collect_crossed_notifies(
    notifies: &[AnimationNotify],
    duration: f32,
    interval: PlaybackInterval,
) -> Result<Vec<CrossedNotify<'_>>, String> {
    let PlaybackInterval::Advance { from, to, looping } = interval else {
        return Ok(Vec::new());
    };
    if !duration.is_finite() || duration < 0.0 || !from.is_finite() || !to.is_finite() {
        return Err("Playback times must be finite and duration nonnegative".into());
    }
    for notify in notifies {
        validate_notify(notify, duration)?;
    }
    if duration == 0.0 || from == to {
        return Ok(Vec::new());
    }
    let duration = duration as f64;
    let (from, to) = if looping {
        (from, to)
    } else {
        (from.clamp(0.0, duration), to.clamp(0.0, duration))
    };
    let forward = to > from;
    let (first, last) = if looping && !notifies.is_empty() {
        candidate_cycles(from, to, duration)?
    } else {
        (0, 0)
    };
    let mut hits = Vec::new();
    for (index, notify) in notifies.iter().enumerate() {
        let offset = notify.time as f64;
        // Each notify has at most a fixed number of excluded edge candidates.
        // Stop at the first excess accepted hit, so even huge intervals have
        // bounded enumeration/allocation without charging excluded departures.
        for cycle in first..=last {
            let time = occurrence_time(cycle, duration, offset);
            let crossed = if forward {
                time > from && time <= to
            } else {
                time >= to && time < from
            };
            if crossed {
                if hits.len() == MAX_NOTIFY_CROSSINGS {
                    return Err(CROSSING_LIMIT_ERROR.into());
                }
                hits.push((
                    index,
                    CrossedNotify {
                        notify,
                        playback_time: time,
                        cycle,
                    },
                ));
            }
        }
    }
    hits.sort_by(|(ai, a), (bi, b)| {
        let time = a.playback_time.total_cmp(&b.playback_time);
        let cycle = a.cycle.cmp(&b.cycle);
        (if forward {
            time.then(cycle)
        } else {
            time.reverse().then(cycle.reverse())
        })
        .then(ai.cmp(bi))
    });
    Ok(hits.into_iter().map(|(_, hit)| hit).collect())
}

/// a validated snapshot for repeated playback queries. rebuild on sequence replacement;
/// edits cannot invalidate its ordering because it owns the authored notifies.
#[derive(Clone, Debug)]
pub struct NotifyIndex {
    notifies: Vec<AnimationNotify>,
    asset_order: Vec<usize>,
    duration: f64,
}

impl NotifyIndex {
    pub fn new(notifies: &[AnimationNotify], duration: f32) -> Result<Self, String> {
        if !duration.is_finite() || duration < 0.0 {
            return Err("Clip duration must be finite and nonnegative".into());
        }
        for notify in notifies {
            validate_notify(notify, duration)?;
        }
        let mut ordered: Vec<_> = notifies.iter().cloned().enumerate().collect();
        ordered.sort_by(|(_, a), (_, b)| a.time.total_cmp(&b.time));
        let (asset_order, notifies) = ordered.into_iter().unzip();
        Ok(Self {
            notifies,
            asset_order,
            duration: duration as f64,
        })
    }

    /// O(log n + crossings) per touched cycle, without revalidating the clip.
    /// rare f64 time collisions sort only the tied window by original asset order.
    pub fn collect(&self, interval: PlaybackInterval) -> Result<Vec<CrossedNotify<'_>>, String> {
        let PlaybackInterval::Advance { from, to, looping } = interval else {
            return Ok(Vec::new());
        };
        if !from.is_finite() || !to.is_finite() {
            return Err("Playback times must be finite".into());
        }
        if self.duration == 0.0 || self.notifies.is_empty() || from == to {
            return Ok(Vec::new());
        }
        let (from, to) = if looping {
            (from, to)
        } else {
            (from.clamp(0.0, self.duration), to.clamp(0.0, self.duration))
        };
        let forward = to > from;
        let (first, last) = if looping {
            candidate_cycles(from, to, self.duration)?
        } else {
            (0, 0)
        };
        let mut hits = Vec::new();
        for step in 0..=last - first {
            let cycle = if forward { first + step } else { last - step };
            let time_of = |index: usize| {
                occurrence_time(cycle, self.duration, self.notifies[index].time as f64)
            };
            // search actual occurrence times to retain boundary precision at large clocks.
            let start = self.notifies.partition_point(|n| {
                if forward {
                    occurrence_time(cycle, self.duration, n.time as f64) <= from
                } else {
                    occurrence_time(cycle, self.duration, n.time as f64) < to
                }
            });
            let end = self.notifies.partition_point(|n| {
                if forward {
                    occurrence_time(cycle, self.duration, n.time as f64) <= to
                } else {
                    occurrence_time(cycle, self.duration, n.time as f64) < from
                }
            });
            let window = &self.notifies[start..end];
            if hits.len() + window.len() > MAX_NOTIFY_CROSSINGS {
                return Err(CROSSING_LIMIT_ERROR.into());
            }
            let (mut lo, mut hi) = (start, end);
            while lo < hi {
                let (start, end) = if forward {
                    let time = time_of(lo);
                    let mut end = lo + 1;
                    while end < hi && time_of(end) == time {
                        end += 1;
                    }
                    let start = lo;
                    lo = end;
                    (start, end)
                } else {
                    let time = time_of(hi - 1);
                    let mut start = hi - 1;
                    while start > lo && time_of(start - 1) == time {
                        start -= 1;
                    }
                    let end = hi;
                    hi = start;
                    (start, end)
                };
                let hit = |index: usize| CrossedNotify {
                    notify: &self.notifies[index],
                    playback_time: time_of(index),
                    cycle,
                };
                if self.asset_order[start..end].is_sorted() {
                    hits.extend((start..end).map(hit));
                } else {
                    // close authored times can become one f64 occurrence at a large clock.
                    let mut tied: Vec<_> = (start..end).collect();
                    tied.sort_unstable_by_key(|&index| self.asset_order[index]);
                    hits.extend(tied.into_iter().map(hit));
                }
            }
        }
        Ok(hits)
    }
}

pub(crate) fn validate_notify(notify: &AnimationNotify, duration: f32) -> Result<(), String> {
    if notify.id.trim().is_empty() || notify.name.trim().is_empty() {
        return Err("Notify ID and name cannot be empty".into());
    }
    if !notify.time.is_finite() || notify.time < 0.0 || notify.time > duration {
        return Err("Notify time must be within the clip".into());
    }
    validate_target_and_fields(&notify.target, &notify.fields)
}

/// The target and free-form payload rules shared by notifies and notify states.
pub(crate) fn validate_target_and_fields(
    target: &NotifyTarget,
    fields: &[NotifyField],
) -> Result<(), String> {
    if let NotifyTarget::ClassVariable(variable) = target {
        if !identifier(variable) {
            return Err("Target must name a class object-reference variable".into());
        }
    }
    let mut names = std::collections::HashSet::new();
    for field in fields {
        if !identifier(&field.name) || RESERVED_NOTIFY_FIELDS.contains(&field.name.as_str()) {
            return Err("Payload field needs a unique, nonreserved identifier".into());
        }
        if !names.insert(&field.name) {
            return Err("Payload field names must be unique".into());
        }
        if matches!(field.value, NotifyValue::Float(v) if !v.is_finite()) {
            return Err("Payload numbers must be finite".into());
        }
    }
    Ok(())
}
