//! Pose Search (motion matching): deterministic index build from a `.posedb`'s
//! sources and a runtime nearest-pose query with a per-channel cost breakdown.
//!
//! Features are always expressed in the root bone's frame, so a pose matches
//! regardless of where or which way the character stands. A built index is
//! derived data: [`PoseIndex::to_payload`] is what the `.pulsar/cache/posedb`
//! file wraps, keyed by the database's settings and source hashes.

use std::sync::Arc;

use glam::{Mat4, Quat, Vec3};

use crate::diagnostics::Diagnostic;
use crate::pose_db::{ChannelKind, Normalization, PoseDbAsset, SearchSettings, TimeRange};

use super::clip::CompiledClip;
use super::pose::{BoneTransform, Curves, Pose, RootMotion, RuntimeSkeleton};

/// A clip the database indexes, after blend-space sources are expanded.
#[derive(Clone, Debug)]
pub struct SearchClip {
    /// Id of the `PoseDbSource` it came from, plus the sample id for blend spaces.
    pub source_id: String,
    pub clip: Arc<CompiledClip>,
    pub mirrored: bool,
    pub looping: bool,
    pub exclusions: Vec<TimeRange>,
    pub cost_bias: f32,
}

/// Left/right bone pairs found by name, for mirrored database sources.
#[derive(Clone, Debug)]
pub struct MirrorTable {
    /// The bone whose motion a bone takes when mirrored (itself if centred).
    pub counterpart: Vec<usize>,
    /// Bones whose names look sided but had no counterpart.
    pub unpaired: Vec<String>,
}

const SIDE_TOKENS: &[(&str, &str)] = &[
    ("Left", "Right"),
    ("left", "right"),
    ("LEFT", "RIGHT"),
    ("_L", "_R"),
    ("_l", "_r"),
    (".L", ".R"),
    (".l", ".r"),
    (" L", " R"),
    ("L_", "R_"),
    ("l_", "r_"),
];

fn flipped(name: &str) -> Option<String> {
    for &(a, b) in SIDE_TOKENS {
        for (from, to) in [(a, b), (b, a)] {
            let short = from.len() <= 2;
            let pos = if short && from.starts_with(['_', '.', ' ']) {
                // suffix tokens: at the end or before another separator
                name.match_indices(from).find(|(i, _)| {
                    let end = i + from.len();
                    end == name.len() || name[end..].starts_with(['_', '.', ' '])
                })
            } else if short {
                // prefix tokens: only at the start
                name.starts_with(from).then_some((0, from))
            } else {
                name.find(from).map(|i| (i, from))
            };
            if let Some((i, _)) = pos {
                return Some(format!("{}{}{}", &name[..i], to, &name[i + from.len()..]));
            }
        }
    }
    None
}

impl MirrorTable {
    pub fn from_names(skel: &RuntimeSkeleton) -> Self {
        let mut counterpart: Vec<usize> = (0..skel.len()).collect();
        let mut unpaired = Vec::new();
        for i in 0..skel.len() {
            let Some(other) = flipped(&skel.names[i]) else { continue };
            match skel.names.iter().position(|n| *n == other) {
                Some(j) => counterpart[i] = j,
                None => unpaired.push(skel.names[i].clone()),
            }
        }
        Self { counterpart, unpaired }
    }

    /// Reflect a pose across the mesh-space YZ plane (x -> -x), swapping each
    /// bone with its counterpart. Root motion mirrors the same way.
    pub fn mirror(&self, skel: &RuntimeSkeleton, pose: &Pose) -> Pose {
        let mesh = skel.to_mesh_space(&pose.bones);
        let reflect = |m: &Mat4| {
            let (s, r, t) = m.to_scale_rotation_translation();
            Mat4::from_scale_rotation_translation(s, Quat::from_xyzw(r.x, -r.y, -r.z, r.w), Vec3::new(-t.x, t.y, t.z))
        };
        let mirrored: Vec<Mat4> = (0..skel.len()).map(|i| reflect(&mesh[self.counterpart[i]])).collect();
        let mut out = Pose { bones: skel.from_mesh_space(&mirrored), curves: pose.curves.clone(), root_motion: pose.root_motion, kind: pose.kind };
        out.root_motion = RootMotion {
            translation: Vec3::new(-pose.root_motion.translation.x, pose.root_motion.translation.y, pose.root_motion.translation.z),
            rotation: Quat::from_xyzw(pose.root_motion.rotation.x, -pose.root_motion.rotation.y, -pose.root_motion.rotation.z, pose.root_motion.rotation.w),
        };
        out
    }
}

/// Where each channel's features sit in an entry's feature vector.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelSpan {
    pub name: String,
    pub start: usize,
    pub len: usize,
    pub weight: f32,
    pub standardize: bool,
}

fn channel_len(kind: &ChannelKind) -> usize {
    match kind {
        ChannelKind::BonePosition { .. } | ChannelKind::BoneVelocity { .. } => 3,
        ChannelKind::BoneRotation { .. } => 4,
        ChannelKind::Trajectory { offsets, position, facing, velocity } => {
            offsets.len() * (3 * (*position as usize + *facing as usize + *velocity as usize))
        }
        ChannelKind::Curve { .. } | ChannelKind::Phase => 1,
    }
}

pub fn layout(asset: &PoseDbAsset) -> Vec<ChannelSpan> {
    let mut start = 0;
    asset
        .channels
        .iter()
        .map(|c| {
            let len = channel_len(&c.kind);
            let span = ChannelSpan { name: c.name.clone(), start, len, weight: c.weight, standardize: c.normalization == Normalization::Standardize };
            start += len;
            span
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IndexEntry {
    pub clip: u32,
    pub time: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PoseIndex {
    pub spans: Vec<ChannelSpan>,
    pub dims: usize,
    pub entries: Vec<IndexEntry>,
    pub features: Vec<f32>,
    pub mean: Vec<f32>,
    pub inv_std: Vec<f32>,
}

/// One frame's pose-derived feature inputs, all in the root bone's frame.
#[derive(Clone, Debug, Default)]
pub struct FrameFeatures {
    pub positions: Vec<Vec3>,
    pub rotations: Vec<Quat>,
    pub velocities: Vec<Vec3>,
    pub curves: Curves,
    pub phase: f32,
}

impl FrameFeatures {
    /// `previous`/`dt` supply bone velocities; without them velocity is zero.
    pub fn from_pose(skel: &RuntimeSkeleton, pose: &Pose, previous: Option<&Pose>, dt: f32, phase: f32) -> Self {
        let (positions, rotations) = root_frame(skel, &pose.bones);
        let velocities = match previous {
            Some(prev) if dt > 1e-6 => {
                let (pp, _) = root_frame(skel, &prev.bones);
                positions.iter().zip(&pp).map(|(a, b)| (*a - *b) / dt).collect()
            }
            _ => vec![Vec3::ZERO; positions.len()],
        };
        Self { positions, rotations, velocities, curves: pose.curves.clone(), phase }
    }
}

fn root_bone(skel: &RuntimeSkeleton) -> usize {
    (0..skel.len()).find(|&i| skel.parents[i].is_none()).unwrap_or(0)
}

fn root_frame(skel: &RuntimeSkeleton, local: &[BoneTransform]) -> (Vec<Vec3>, Vec<Quat>) {
    let mesh = skel.to_mesh_space(local);
    let inv = mesh[root_bone(skel)].inverse();
    let mut pos = Vec::with_capacity(skel.len());
    let mut rot = Vec::with_capacity(skel.len());
    for m in &mesh {
        let (_, r, t) = (inv * *m).to_scale_rotation_translation();
        pos.push(t);
        rot.push(if r.w < 0.0 { -r } else { r });
    }
    (pos, rot)
}

/// A desired trajectory for a query: constant velocity in the actor's frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DesiredTrajectory {
    pub velocity: Vec3,
}

fn push_trajectory(out: &mut Vec<f32>, position: bool, facing: bool, velocity: bool, p: Vec3, f: Vec3, v: Vec3) {
    if position {
        out.extend_from_slice(&p.to_array());
    }
    if facing {
        out.extend_from_slice(&f.to_array());
    }
    if velocity {
        out.extend_from_slice(&v.to_array());
    }
}

fn bone_for(skel: &RuntimeSkeleton, key: &str) -> Option<usize> {
    skel.bone_by_id_or_name(key)
}

impl PoseIndex {
    /// Build the index. Bones and curves named by channels must exist (the
    /// first problem is returned); an empty database is an error rather than a
    /// silently empty index. Entries are generated in source order, then time.
    pub fn build(
        asset: &PoseDbAsset,
        skel: &RuntimeSkeleton,
        clips: &[SearchClip],
        mirror: Option<&MirrorTable>,
    ) -> Result<Self, Vec<Diagnostic>> {
        let mut diagnostics = Vec::new();
        if asset.channels.is_empty() {
            diagnostics.push(Diagnostic::error("posedb.no_channels", "The database has no feature channels").repair("Add at least one feature channel"));
        }
        for c in &asset.channels {
            match &c.kind {
                ChannelKind::BonePosition { bone } | ChannelKind::BoneRotation { bone } | ChannelKind::BoneVelocity { bone } if bone_for(skel, bone).is_none() => {
                    diagnostics.push(Diagnostic::error("posedb.unknown_bone", format!("Channel `{}` names bone `{bone}` which the skeleton does not have", c.name)).at(&c.id));
                }
                _ => {}
            }
        }
        if clips.is_empty() {
            diagnostics.push(Diagnostic::error("posedb.no_sources", "The database has no usable source clips").repair("Add or enable a source sequence"));
        }
        if clips.iter().any(|c| c.mirrored) && mirror.is_none() {
            diagnostics.push(Diagnostic::error("posedb.mirror_unavailable", "A mirrored source needs a left/right bone table"));
        }
        if !diagnostics.is_empty() {
            return Err(diagnostics);
        }
        let spans = layout(asset);
        let dims: usize = spans.iter().map(|s| s.len).sum();
        let interval = asset.sampling.interval.max(1.0 / 240.0);
        let mut entries = Vec::new();
        let mut features = Vec::new();
        for (ci, sc) in clips.iter().enumerate() {
            let dur = sc.clip.duration;
            let limit = if sc.looping { dur } else { dur - asset.sampling.end_margin };
            let mut k = 0u32;
            loop {
                let t = k as f32 * interval;
                if t > limit + 1e-6 || (k > 0 && dur <= 0.0) {
                    break;
                }
                k += 1;
                if sc.exclusions.iter().any(|r| t >= r.start && t < r.end) {
                    continue;
                }
                let tt = t.min(dur);
                let f = Self::clip_features(asset, skel, sc, tt, interval, mirror);
                debug_assert_eq!(f.len(), dims);
                entries.push(IndexEntry { clip: ci as u32, time: tt });
                features.extend(f);
            }
        }
        if entries.is_empty() {
            return Err(vec![Diagnostic::error("posedb.empty", "No pose could be indexed: every source is excluded or shorter than the end margin").repair("Check source exclusions and the sampling end margin")]);
        }
        let n = entries.len();
        let (mut mean, mut inv_std) = (vec![0.0f32; dims], vec![1.0f32; dims]);
        for span in spans.iter().filter(|s| s.standardize) {
            for d in span.start..span.start + span.len {
                let m = (0..n).map(|e| features[e * dims + d] as f64).sum::<f64>() / n as f64;
                let var = (0..n).map(|e| (features[e * dims + d] as f64 - m).powi(2)).sum::<f64>() / n as f64;
                mean[d] = m as f32;
                let sd = var.sqrt() as f32;
                inv_std[d] = if sd > 1e-6 { 1.0 / sd } else { 1.0 };
            }
        }
        Ok(Self { spans, dims, entries, features, mean, inv_std })
    }

    fn clip_features(asset: &PoseDbAsset, skel: &RuntimeSkeleton, sc: &SearchClip, t: f32, interval: f32, mirror: Option<&MirrorTable>) -> Vec<f32> {
        let sample = |time: f32| {
            let pose = sc.clip.sample(skel, time);
            if sc.mirrored { mirror.expect("checked").mirror(skel, &pose) } else { pose }
        };
        let h = (interval * 0.5).max(1.0 / 480.0);
        let pose = sample(t);
        let prev = sample((t - h).max(0.0));
        let frame = FrameFeatures::from_pose(skel, &pose, Some(&prev), (t - (t - h).max(0.0)).max(1e-6), if sc.clip.duration > 0.0 { t / sc.clip.duration } else { 0.0 });
        let mut out = Vec::new();
        for c in &asset.channels {
            match &c.kind {
                ChannelKind::Trajectory { offsets, position, facing, velocity } => {
                    for &o in offsets {
                        let traj = |dt: f32| {
                            let m = sc.clip.trajectory_between(skel, t as f64, (t + dt) as f64, sc.looping);
                            if sc.mirrored {
                                RootMotion {
                                    translation: Vec3::new(-m.translation.x, m.translation.y, m.translation.z),
                                    rotation: Quat::from_xyzw(m.rotation.x, -m.rotation.y, -m.rotation.z, m.rotation.w),
                                }
                            } else {
                                m
                            }
                        };
                        let at = traj(o);
                        let facing_v = at.rotation * Vec3::Z;
                        let vel = (traj(o + h).translation - traj(o - h).translation) / (2.0 * h);
                        push_trajectory(&mut out, *position, *facing, *velocity, at.translation, facing_v, vel);
                    }
                }
                other => Self::push_pose_channel(&mut out, skel, other, &frame),
            }
        }
        out
    }

    fn push_pose_channel(out: &mut Vec<f32>, skel: &RuntimeSkeleton, kind: &ChannelKind, f: &FrameFeatures) {
        match kind {
            ChannelKind::BonePosition { bone } => out.extend_from_slice(&f.positions[bone_for(skel, bone).unwrap_or(0)].to_array()),
            ChannelKind::BoneRotation { bone } => out.extend_from_slice(&f.rotations[bone_for(skel, bone).unwrap_or(0)].to_array()),
            ChannelKind::BoneVelocity { bone } => out.extend_from_slice(&f.velocities[bone_for(skel, bone).unwrap_or(0)].to_array()),
            ChannelKind::Curve { curve } => out.push(f.curves.get(curve).copied().unwrap_or(0.0)),
            ChannelKind::Phase => out.push(f.phase),
            ChannelKind::Trajectory { .. } => {}
        }
    }

    /// Feature vector for a runtime query: pose channels from `frame`,
    /// trajectory channels from the desired constant velocity (past offsets
    /// assume the character has been moving the same way).
    pub fn query_vector(&self, asset: &PoseDbAsset, skel: &RuntimeSkeleton, frame: &FrameFeatures, desired: &DesiredTrajectory) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.dims);
        let speed = desired.velocity.length();
        let facing = if speed > 1e-4 { desired.velocity / speed } else { Vec3::Z };
        for c in &asset.channels {
            match &c.kind {
                ChannelKind::Trajectory { offsets, position, facing: want_facing, velocity } => {
                    for &o in offsets {
                        push_trajectory(&mut out, *position, *want_facing, *velocity, desired.velocity * o, facing, desired.velocity);
                    }
                }
                other => Self::push_pose_channel(&mut out, skel, other, frame),
            }
        }
        out
    }

    fn normalised(&self, d: usize, v: f32, standardize: bool) -> f32 {
        if standardize { (v - self.mean[d]) * self.inv_std[d] } else { v }
    }

    /// Cost of one entry against `query`, with the contribution of each channel.
    pub fn cost(&self, entry: usize, query: &[f32]) -> (f32, Vec<f32>) {
        let row = &self.features[entry * self.dims..(entry + 1) * self.dims];
        let mut per = Vec::with_capacity(self.spans.len());
        let mut total = 0.0;
        for s in &self.spans {
            if s.len == 0 {
                per.push(0.0);
                continue;
            }
            let mut sum = 0.0;
            for d in s.start..s.start + s.len {
                let diff = self.normalised(d, row[d], s.standardize) - self.normalised(d, query[d], s.standardize);
                sum += diff * diff;
            }
            let c = s.weight * sum / s.len as f32;
            per.push(c);
            total += c;
        }
        (total, per)
    }

    /// Best matches for `query`. `current` is the `(clip, time)` being played:
    /// the nearest entry of that clip is the continuing pose and gets the
    /// bias discount. Ties break toward the lower entry index.
    pub fn search(
        &self,
        clips: &[SearchClip],
        query: &[f32],
        current: Option<(u32, f32)>,
        node_bias: f32,
        settings: &SearchSettings,
    ) -> Option<SearchResult> {
        if self.entries.is_empty() || query.len() != self.dims {
            return None;
        }
        let continuing = current.and_then(|(clip, time)| {
            self.entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.clip == clip)
                .min_by(|a, b| (a.1.time - time).abs().total_cmp(&(b.1.time - time).abs()).then(a.0.cmp(&b.0)))
                .map(|(i, _)| i)
        });
        let mut scored: Vec<(f32, usize, Vec<f32>)> = Vec::with_capacity(self.entries.len());
        for i in 0..self.entries.len() {
            let (mut c, per) = self.cost(i, query);
            c += clips[self.entries[i].clip as usize].cost_bias;
            if continuing == Some(i) {
                c = (c * (1.0 - node_bias.clamp(0.0, 1.0)) - settings.continuing_pose_bias.max(0.0)).max(0.0);
            }
            scored.push((c, i, per));
        }
        scored.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let (cost, entry, breakdown) = scored[0].clone();
        let e = self.entries[entry];
        Some(SearchResult {
            entry,
            clip: e.clip,
            clip_time: e.time,
            mirrored: clips[e.clip as usize].mirrored,
            cost,
            breakdown: self.spans.iter().zip(breakdown).map(|(s, c)| ChannelCost { channel: s.name.clone(), cost: c }).collect(),
            continuing: continuing == Some(entry),
            candidates: scored.iter().take(settings.candidate_count.max(1) as usize).map(|(c, i, _)| (*i, *c)).collect(),
        })
    }

    /// The bytes a derived-index cache file wraps.
    pub fn to_payload(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let put32 = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
        put32(&mut out, self.dims as u32);
        put32(&mut out, self.entries.len() as u32);
        for e in &self.entries {
            put32(&mut out, e.clip);
            out.extend_from_slice(&e.time.to_le_bytes());
        }
        for v in self.features.iter().chain(&self.mean).chain(&self.inv_std) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// Rebuild from [`Self::to_payload`] for the database `asset` (whose channel
    /// layout must match; the cache key guarantees it).
    pub fn from_payload(asset: &PoseDbAsset, bytes: &[u8]) -> Option<Self> {
        let spans = layout(asset);
        let want_dims: usize = spans.iter().map(|s| s.len).sum();
        let u32_at = |o: usize| bytes.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let f32_at = |o: usize| bytes.get(o..o + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap()));
        let dims = u32_at(0)? as usize;
        let n = u32_at(4)? as usize;
        if dims != want_dims || bytes.len() != 8 + n * 8 + (n * dims + 2 * dims) * 4 {
            return None;
        }
        let mut o = 8;
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            entries.push(IndexEntry { clip: u32_at(o)?, time: f32_at(o + 4)? });
            o += 8;
        }
        let mut take = |count: usize| -> Option<Vec<f32>> {
            let v = (0..count).map(|i| f32_at(o + i * 4)).collect::<Option<Vec<_>>>()?;
            o += count * 4;
            Some(v)
        };
        let features = take(n * dims)?;
        let mean = take(dims)?;
        let inv_std = take(dims)?;
        Some(Self { spans, dims, entries, features, mean, inv_std })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelCost {
    pub channel: String,
    pub cost: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchResult {
    pub entry: usize,
    pub clip: u32,
    pub clip_time: f32,
    pub mirrored: bool,
    pub cost: f32,
    pub breakdown: Vec<ChannelCost>,
    /// The match was the pose already playing.
    pub continuing: bool,
    /// Best `candidate_count` entries as `(entry, cost)`.
    pub candidates: Vec<(usize, f32)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pose_db::{FeatureChannel, PoseDbSource, PoseSource};
    use crate::runtime::clip::tests::{clip_asset, translate_track};
    use crate::runtime::pose::tests::chain_skeleton;

    fn setup() -> (RuntimeSkeleton, Vec<SearchClip>, PoseDbAsset) {
        let skel = RuntimeSkeleton::new("s", &chain_skeleton(3)).unwrap();
        // "walk": b2 oscillates +x over time and the root walks forward; "idle": still
        let mut walk = clip_asset("walk", 2.0, vec![translate_track("b0", [0.0; 3], [0.0, 0.0, 4.0], 2.0), translate_track("b2", [0.0, 1.0, 0.0], [1.0, 1.0, 0.0], 2.0)]);
        walk.features.root_motion.enabled = true;
        let idle = clip_asset("idle", 2.0, vec![]);
        let clips = vec![
            SearchClip { source_id: "walk".into(), clip: Arc::new(CompiledClip::compile(&walk, &skel)), mirrored: false, looping: false, exclusions: vec![], cost_bias: 0.0 },
            SearchClip { source_id: "idle".into(), clip: Arc::new(CompiledClip::compile(&idle, &skel)), mirrored: false, looping: false, exclusions: vec![], cost_bias: 0.0 },
        ];
        let mut db = PoseDbAsset::new("db", None);
        db.sampling.interval = 0.25;
        db.sampling.end_margin = 0.5;
        db.sources.push(PoseDbSource { id: "w".into(), source: PoseSource::Sequence { sequence: pulsar_reflection::AssetRef::new() }, enabled: true, mirrored: false, looping: false, exclusions: vec![], cost_bias: 0.0 });
        db.channels.push(FeatureChannel { id: "p".into(), name: "hand".into(), weight: 1.0, normalization: Normalization::Standardize, kind: ChannelKind::BonePosition { bone: "b2".into() } });
        db.channels.push(FeatureChannel {
            id: "t".into(), name: "traj".into(), weight: 2.0, normalization: Normalization::Standardize,
            kind: ChannelKind::Trajectory { offsets: vec![0.5], position: true, facing: false, velocity: false },
        });
        (skel, clips, db)
    }

    #[test]
    fn build_is_deterministic_and_the_payload_round_trips() {
        let (skel, clips, db) = setup();
        let a = PoseIndex::build(&db, &skel, &clips, None).unwrap();
        let b = PoseIndex::build(&db, &skel, &clips, None).unwrap();
        assert_eq!(a, b);
        // 2 s clips, 0.25 s interval, 0.5 s end margin: t = 0..=1.5 -> 7 entries each
        assert_eq!(a.entries.len(), 14);
        assert_eq!(a.dims, 6);
        let back = PoseIndex::from_payload(&db, &a.to_payload()).unwrap();
        assert_eq!(a, back);
        assert!(PoseIndex::from_payload(&db, &a.to_payload()[..20]).is_none());
        // editing a search setting is not a build input; a channel is
        let mut changed = db.clone();
        changed.channels[0].weight = 5.0;
        assert_ne!(db.settings_hash(), changed.settings_hash());
        let mut tuned = db.clone();
        tuned.search.blend_time = 9.0;
        assert_eq!(db.settings_hash(), tuned.settings_hash());
    }

    #[test]
    fn query_finds_the_matching_pose_with_a_cost_breakdown_and_fallback_on_bad_queries() {
        let (skel, clips, db) = setup();
        let index = PoseIndex::build(&db, &skel, &clips, None).unwrap();
        // the walk's pose at t=1.0: hand at x=0.5, moving +z at 2/s -> 1.0 m ahead in 0.5 s
        let pose = clips[0].clip.sample(&skel, 1.0);
        let frame = FrameFeatures::from_pose(&skel, &pose, None, 0.0, 0.5);
        let q = index.query_vector(&db, &skel, &frame, &DesiredTrajectory { velocity: Vec3::new(0.0, 0.0, 2.0) });
        let r = index.search(&clips, &q, None, 0.0, &db.search).unwrap();
        assert_eq!(r.clip, 0);
        assert!((r.clip_time - 1.0).abs() < 0.26, "{}", r.clip_time);
        assert_eq!(r.breakdown.len(), 2);
        assert!((r.breakdown.iter().map(|c| c.cost).sum::<f32>() - r.cost).abs() < 1e-4);
        // standing still with the hand at rest matches idle
        let idle = clips[1].clip.sample(&skel, 0.5);
        let frame = FrameFeatures::from_pose(&skel, &idle, None, 0.0, 0.0);
        let q = index.query_vector(&db, &skel, &frame, &DesiredTrajectory { velocity: Vec3::ZERO });
        assert_eq!(index.search(&clips, &q, None, 0.0, &db.search).unwrap().clip, 1);
        // wrong-size queries find nothing, so the caller plays its fallback
        assert!(index.search(&clips, &[0.0], None, 0.0, &db.search).is_none());
    }

    #[test]
    fn continuing_bias_prefers_staying_in_the_clip_and_ties_are_stable() {
        let (skel, clips, db) = setup();
        let index = PoseIndex::build(&db, &skel, &clips, None).unwrap();
        let pose = clips[0].clip.sample(&skel, 1.0);
        let frame = FrameFeatures::from_pose(&skel, &pose, None, 0.0, 0.5);
        let q = index.query_vector(&db, &skel, &frame, &DesiredTrajectory { velocity: Vec3::new(0.0, 0.0, 2.0) });
        let free = index.search(&clips, &q, Some((0, 0.0)), 0.0, &db.search).unwrap();
        let biased = index.search(&clips, &q, Some((0, 1.0)), 0.9, &db.search).unwrap();
        assert!(biased.cost <= free.cost);
        assert!(biased.continuing || biased.clip == 0);
        assert_eq!(free, index.search(&clips, &q, Some((0, 0.0)), 0.0, &db.search).unwrap());
        // two identical clips tie: the first wins
        let twin = vec![clips[1].clone(), clips[1].clone()];
        let idx = PoseIndex::build(&db, &skel, &twin, None).unwrap();
        let q0 = idx.features[..idx.dims].to_vec();
        assert_eq!(idx.search(&twin, &q0, None, 0.0, &db.search).unwrap().clip, 0);
    }

    #[test]
    fn invalid_databases_report_instead_of_building_something_empty() {
        let (skel, clips, mut db) = setup();
        let mut no_channels = db.clone();
        no_channels.channels.clear();
        assert!(PoseIndex::build(&no_channels, &skel, &clips, None).unwrap_err().iter().any(|d| d.code == "posedb.no_channels"));
        db.channels[0].kind = ChannelKind::BonePosition { bone: "missing".into() };
        assert!(PoseIndex::build(&db, &skel, &clips, None).unwrap_err().iter().any(|d| d.code == "posedb.unknown_bone"));
        let (skel, clips, mut db) = setup();
        db.sampling.end_margin = 5.0;
        assert!(PoseIndex::build(&db, &skel, &clips, None).unwrap_err().iter().any(|d| d.code == "posedb.empty"));
        let (skel, _, db) = setup();
        assert!(PoseIndex::build(&db, &skel, &[], None).unwrap_err().iter().any(|d| d.code == "posedb.no_sources"));
    }

    #[test]
    fn mirrored_sources_flip_left_and_right_by_name() {
        let skeleton = crate::skeleton::Skeleton {
            bones: ["root", "arm_l", "arm_r"]
                .iter()
                .enumerate()
                .map(|(i, n)| crate::skeleton::Bone {
                    id: n.to_string(),
                    name: n.to_string(),
                    parent: (i > 0).then(|| "root".to_string()),
                    bind_transform: crate::math::Transform {
                        translation: crate::math::Vec3::new(match i { 1 => 1.0, 2 => -1.0, _ => 0.0 }, 0.0, 0.0),
                        ..crate::math::Transform::IDENTITY
                    },
                })
                .collect(),
        };
        let skel = RuntimeSkeleton::new("m", &skeleton).unwrap();
        let table = MirrorTable::from_names(&skel);
        assert_eq!(table.counterpart, [0, 2, 1]);
        assert!(table.unpaired.is_empty());
        let mut pose = skel.bind_pose();
        pose.bones[1].translation = Vec3::new(1.0, 0.5, 0.0); // left arm raised
        pose.root_motion.translation = Vec3::new(1.0, 0.0, 2.0);
        let m = table.mirror(&skel, &pose);
        assert!((m.bones[2].translation - Vec3::new(-1.0, 0.5, 0.0)).length() < 1e-5, "right arm raised: {:?}", m.bones[2].translation);
        assert!((m.bones[1].translation - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-5);
        assert_eq!(m.root_motion.translation, Vec3::new(-1.0, 0.0, 2.0));
        // mirroring twice is the identity
        let back = table.mirror(&skel, &m);
        for (a, b) in back.bones.iter().zip(&pose.bones) {
            assert!((a.translation - b.translation).length() < 1e-5);
        }
        assert_eq!(flipped("LeftHand").as_deref(), Some("RightHand"));
        assert_eq!(flipped("hand.R").as_deref(), Some("hand.L"));
        assert_eq!(flipped("spine"), None);
    }
}
