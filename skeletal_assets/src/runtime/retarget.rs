//! Retarget execution: a Retarget Profile (`.animretarget`) or an IK Retargeter
//! (`.ikretarget`) compiled into one plan that maps a pose on the source
//! skeleton onto the target skeleton.
//!
//! Rotations travel as mesh-space deltas from the source retarget pose and are
//! applied on the target retarget pose, so two characters in different
//! rest poses still agree on what "arm raised" means. Translation modes and the
//! root policy decide how positions follow. Mesh space is Y-up.

use std::sync::Arc;

use glam::{Mat4, Quat, Vec3};

use crate::diagnostics::Diagnostic;
use crate::ik_retarget::IkRetargetAsset;
use crate::retarget::{RetargetPose, RetargetProfileAsset, RootScale, RotationMode, TranslationMode};

use super::ik::Effector;
use super::ikrig::{CompiledIkRig, GoalSet};
use super::pose::{slerp_short, BoneTransform, Pose, RootMotion, RuntimeSkeleton};

#[derive(Clone, Debug)]
struct ChainPair {
    name: String,
    src: Vec<usize>,
    tgt: Vec<usize>,
    rotation: RotationMode,
    rotation_alpha: f32,
    translation: TranslationMode,
    translation_alpha: f32,
}

#[derive(Clone, Debug)]
struct RootPlan {
    src: usize,
    tgt: usize,
    translation_alpha: f32,
    rotation_alpha: f32,
    scale_h: f32,
    scale_v: f32,
    offset: Vec3,
    /// Added to the target root after retargeting (retarget pose root offset).
    pose_offset: Vec3,
}

#[derive(Clone, Debug)]
struct IkCorrection {
    /// Index of the target chain in the target rig.
    chain: usize,
    /// Index of the source chain in the source rig.
    source_chain: usize,
    blend: f32,
    static_offset: Vec3,
    scale_vertical: f32,
    extension: f32,
}

#[derive(Clone, Debug)]
pub struct RetargetPlan {
    pub source: Arc<RuntimeSkeleton>,
    pub target: Arc<RuntimeSkeleton>,
    pairs: Vec<ChainPair>,
    root: Option<RootPlan>,
    ratio: f32,
    src_pose: Vec<BoneTransform>,
    tgt_pose: Vec<BoneTransform>,
    src_pose_mesh: Vec<Mat4>,
    tgt_pose_mesh: Vec<Mat4>,
    ik: Vec<IkCorrection>,
    target_rig: Option<Arc<CompiledIkRig>>,
    source_rig: Option<Arc<CompiledIkRig>>,
    pub diagnostics: Vec<Diagnostic>,
}

fn apply_pose(skel: &RuntimeSkeleton, pose: &RetargetPose) -> Vec<BoneTransform> {
    let mut local = skel.bind.clone();
    for offset in &pose.bone_rotations {
        if let Some(i) = skel.bone(&offset.bone) {
            let q = Quat::from_array(offset.rotation).normalize();
            local[i].rotation = (local[i].rotation * q).normalize();
        }
    }
    local
}

fn size_of(skel: &RuntimeSkeleton, mesh: &[Mat4], root: usize) -> f32 {
    let origin = mesh[root].to_scale_rotation_translation().2;
    (0..skel.len()).map(|i| (mesh[i].to_scale_rotation_translation().2 - origin).length()).fold(0.0, f32::max)
}

impl RetargetPlan {
    fn base(
        source: Arc<RuntimeSkeleton>,
        target: Arc<RuntimeSkeleton>,
        src_pose: &RetargetPose,
        tgt_pose: &RetargetPose,
    ) -> Self {
        let src_local = apply_pose(&source, src_pose);
        let tgt_local = apply_pose(&target, tgt_pose);
        let src_mesh = source.to_mesh_space(&src_local);
        let tgt_mesh = target.to_mesh_space(&tgt_local);
        Self {
            source,
            target,
            pairs: Vec::new(),
            root: None,
            ratio: 1.0,
            src_pose: src_local,
            tgt_pose: tgt_local,
            src_pose_mesh: src_mesh,
            tgt_pose_mesh: tgt_mesh,
            ik: Vec::new(),
            target_rig: None,
            source_rig: None,
            diagnostics: Vec::new(),
        }
    }

    /// Plan for a Retarget Profile. With both rigs the profile maps by chain,
    /// otherwise by bone.
    pub fn from_profile(
        asset: &RetargetProfileAsset,
        source: Arc<RuntimeSkeleton>,
        target: Arc<RuntimeSkeleton>,
        rigs: Option<(Arc<CompiledIkRig>, Arc<CompiledIkRig>)>,
    ) -> Self {
        let mut plan = Self::base(source, target, &asset.source_pose, &asset.target_pose);
        if asset.maps_by_chain() {
            match &rigs {
                Some((source_rig, target_rig)) => {
                    for m in &asset.chain_map {
                        let (Some(s), Some(t)) = (source_rig.chain_index(&m.source_chain), target_rig.chain_index(&m.target_chain)) else {
                            plan.diagnostics.push(Diagnostic::error("retarget.unknown_chain", format!("Chain mapping `{}` names a chain its rig does not have", m.id)).at(&m.id));
                            continue;
                        };
                        let (sb, tb) = (source_rig.chains[s].bones.clone(), target_rig.chains[t].bones.clone());
                        if sb.is_empty() || tb.is_empty() {
                            plan.diagnostics.push(Diagnostic::error("retarget.chain_unresolved", format!("Chain mapping `{}` has a chain that does not resolve to bones", m.id)).at(&m.id));
                            continue;
                        }
                        plan.pairs.push(ChainPair {
                            name: target_rig.chains[t].name.clone(),
                            src: sb,
                            tgt: tb,
                            rotation: m.rotation,
                            rotation_alpha: m.rotation_alpha,
                            translation: m.translation,
                            translation_alpha: m.translation_alpha,
                        });
                    }
                }
                None => plan.diagnostics.push(
                    Diagnostic::error("retarget.rigs_missing", "The profile maps by chain but its IK rigs could not be loaded")
                        .repair("Restore the source and target IK rigs"),
                ),
            }
        } else {
            for m in &asset.bone_map {
                let (Some(s), Some(t)) = (plan.source.bone(&m.source_bone), plan.target.bone(&m.target_bone)) else {
                    plan.diagnostics.push(Diagnostic::error("retarget.unknown_bone", format!("Bone mapping `{}` names a bone its skeleton does not have", m.id)).at(&m.id));
                    continue;
                };
                plan.pairs.push(ChainPair {
                    name: plan.target.names[t].clone(),
                    src: vec![s],
                    tgt: vec![t],
                    rotation: m.rotation,
                    rotation_alpha: 1.0,
                    translation: m.translation,
                    translation_alpha: 1.0,
                });
            }
        }
        let src_root = asset.root.source_root.as_deref().and_then(|b| plan.source.bone(b));
        let tgt_root = asset.root.target_root.as_deref().and_then(|b| plan.target.bone(b));
        if let (Some(src), Some(tgt)) = (src_root, tgt_root) {
            let size_s = size_of(&plan.source, &plan.src_pose_mesh, src);
            let size_t = size_of(&plan.target, &plan.tgt_pose_mesh, tgt);
            plan.ratio = match asset.root.scale {
                RootScale::Fixed { factor } => factor,
                RootScale::Auto if size_s > 1e-6 && size_t > 1e-6 => size_t / size_s,
                RootScale::Auto => 1.0,
            };
            plan.root = Some(RootPlan {
                src,
                tgt,
                translation_alpha: asset.root.translation_alpha,
                rotation_alpha: asset.root.rotation_alpha,
                scale_h: 1.0,
                scale_v: 1.0,
                offset: Vec3::ZERO,
                pose_offset: Vec3::from_array(asset.target_pose.root_translation),
            });
        } else {
            plan.diagnostics.push(Diagnostic::warning("retarget.no_root", "Root bones are not set; root translation and root motion are not retargeted").repair("Pick the source and target root bones"));
        }
        plan
    }

    /// Plan for an IK Retargeter: FK per chain pair from the rigs' chains by
    /// name, then optional IK goals through the target rig.
    pub fn from_ik_retargeter(
        asset: &IkRetargetAsset,
        source: Arc<RuntimeSkeleton>,
        target: Arc<RuntimeSkeleton>,
        source_rig: Arc<CompiledIkRig>,
        target_rig: Arc<CompiledIkRig>,
    ) -> Self {
        let mut plan = Self::base(source, target, &asset.source_pose, &asset.target_pose);
        for c in &asset.chains {
            let Some(t) = target_rig.chain_index(&c.target_chain) else {
                plan.diagnostics.push(Diagnostic::error("ikretarget.unknown_chain", format!("Target chain `{}` is not in the target rig", c.target_chain)).at(&c.id));
                continue;
            };
            let Some(source_name) = &c.source_chain else {
                plan.diagnostics.push(Diagnostic::warning("ikretarget.unmapped_chain", format!("Target chain `{}` has no source and holds its retarget pose", c.target_chain)).at(&c.id).repair("Pick a source chain"));
                continue;
            };
            let Some(s) = source_rig.chain_index(source_name) else {
                plan.diagnostics.push(Diagnostic::error("ikretarget.unknown_chain", format!("Source chain `{source_name}` is not in the source rig")).at(&c.id));
                continue;
            };
            let (sb, tb) = (source_rig.chains[s].bones.clone(), target_rig.chains[t].bones.clone());
            if sb.is_empty() || tb.is_empty() {
                plan.diagnostics.push(Diagnostic::error("ikretarget.chain_unresolved", format!("Chain `{}` does not resolve to bones", c.target_chain)).at(&c.id));
                continue;
            }
            if c.fk.enabled {
                plan.pairs.push(ChainPair {
                    name: c.target_chain.clone(),
                    src: sb,
                    tgt: tb,
                    rotation: c.fk.rotation,
                    rotation_alpha: c.fk.rotation_alpha,
                    translation: c.fk.translation,
                    translation_alpha: c.fk.translation_alpha,
                });
            }
            if c.ik.enabled {
                if target_rig.chains[t].goal.is_none() {
                    plan.diagnostics.push(Diagnostic::error("ikretarget.no_goal", format!("Chain `{}` has IK enabled but the target rig gives it no goal", c.target_chain)).at(&c.id).repair("Assign a goal to the chain in the IK rig"));
                } else {
                    plan.ik.push(IkCorrection {
                        chain: t,
                        source_chain: s,
                        blend: c.ik.blend_to_source,
                        static_offset: Vec3::from_array(c.ik.static_offset),
                        scale_vertical: c.ik.scale_vertical,
                        extension: c.ik.extension,
                    });
                }
            }
        }
        let root_pair = source_rig.retarget_root.zip(target_rig.retarget_root);
        if let Some((src, tgt)) = root_pair {
            let size_s = size_of(&plan.source, &plan.src_pose_mesh, src);
            let size_t = size_of(&plan.target, &plan.tgt_pose_mesh, tgt);
            plan.ratio = if size_s > 1e-6 && size_t > 1e-6 { size_t / size_s } else { 1.0 };
            plan.root = Some(RootPlan {
                src,
                tgt,
                translation_alpha: asset.root.translation_alpha,
                rotation_alpha: asset.root.rotation_alpha,
                scale_h: asset.root.scale_horizontal,
                scale_v: asset.root.scale_vertical,
                offset: Vec3::from_array(asset.root.offset),
                pose_offset: Vec3::from_array(asset.target_pose.root_translation),
            });
        } else {
            plan.diagnostics.push(Diagnostic::warning("ikretarget.no_root", "Both rigs need a retarget root for root translation to be retargeted").repair("Set the retarget root on the source and target IK rigs"));
        }
        plan.target_rig = Some(target_rig);
        plan.source_rig = Some(source_rig);
        plan
    }

    /// The auto root scale (target size over source size).
    pub fn scale_ratio(&self) -> f32 {
        self.ratio
    }

    /// Map `src` (a full pose on the source skeleton) onto the target skeleton.
    pub fn retarget(&self, src: &Pose) -> Pose {
        let (source, target) = (&*self.source, &*self.target);
        let src_mesh = source.to_mesh_space(&src.bones);
        let src_rot = |i: usize| src_mesh[i].to_scale_rotation_translation().1;
        let pose_rot = |i: usize| self.src_pose_mesh[i].to_scale_rotation_translation().1;
        let delta = |i: usize| (src_rot(i) * pose_rot(i).inverse()).normalize();

        let n = target.len();
        let mut want_mesh_rot: Vec<Option<Quat>> = vec![None; n];
        let mut want_translation: Vec<Option<Vec3>> = vec![None; n];
        let tgt_rot = |i: usize| self.tgt_pose_mesh[i].to_scale_rotation_translation().1;

        for pair in &self.pairs {
            let (ns, nt) = (pair.src.len(), pair.tgt.len());
            let along = |skel: &RuntimeSkeleton, mesh: &[Mat4], bones: &[usize]| -> Vec<f32> {
                let mut acc = vec![0.0f32];
                for w in bones.windows(2) {
                    let d = (mesh[w[1]].to_scale_rotation_translation().2 - mesh[w[0]].to_scale_rotation_translation().2).length();
                    acc.push(acc.last().unwrap() + d);
                }
                let total = acc.last().copied().unwrap_or(0.0);
                let _ = skel;
                acc.iter().map(|d| if total > 1e-9 { d / total } else { 0.0 }).collect()
            };
            let (src_u, tgt_u) = (along(source, &self.src_pose_mesh, &pair.src), along(target, &self.tgt_pose_mesh, &pair.tgt));
            for (j, &t) in pair.tgt.iter().enumerate() {
                let proportional = if nt <= 1 { 0 } else { ((j as f32) * (ns - 1) as f32 / (nt - 1) as f32).round() as usize };
                let s_idx = match pair.rotation {
                    RotationMode::OneToOneReversed => ns - 1 - proportional.min(ns - 1),
                    _ => proportional.min(ns - 1),
                };
                let chain_delta = match pair.rotation {
                    RotationMode::None => None,
                    RotationMode::OneToOne | RotationMode::OneToOneReversed => Some(delta(pair.src[s_idx])),
                    RotationMode::Interpolated => {
                        if ns == 1 {
                            Some(delta(pair.src[0]))
                        } else {
                            let u = tgt_u[j];
                            let hi = src_u.partition_point(|&x| x <= u).clamp(1, ns - 1);
                            let (lo_u, hi_u) = (src_u[hi - 1], src_u[hi]);
                            let w = if hi_u - lo_u > 1e-9 { ((u - lo_u) / (hi_u - lo_u)).clamp(0.0, 1.0) } else { 0.0 };
                            Some(slerp_short(delta(pair.src[hi - 1]), delta(pair.src[hi]), w))
                        }
                    }
                };
                if let Some(d) = chain_delta {
                    let full = (d * tgt_rot(t)).normalize();
                    want_mesh_rot[t] = Some(slerp_short(tgt_rot(t), full, pair.rotation_alpha));
                }
                let s = pair.src[s_idx];
                let rest = self.tgt_pose[t].translation;
                let value = match pair.translation {
                    TranslationMode::None => None,
                    TranslationMode::GloballyScaled => Some(rest + (src.bones[s].translation - self.src_pose[s].translation) * self.ratio),
                    TranslationMode::Absolute => Some(src.bones[s].translation),
                };
                if let Some(v) = value {
                    want_translation[t] = Some(rest.lerp(v, pair.translation_alpha));
                }
            }
        }

        // root: mesh-space translation follows the source root, scaled
        let mut root_mesh_pos: Option<(usize, Vec3)> = None;
        if let Some(root) = &self.root {
            let src_pos = src_mesh[root.src].to_scale_rotation_translation().2;
            let rest_pos = self.src_pose_mesh[root.src].to_scale_rotation_translation().2;
            let mut d = (src_pos - rest_pos) * self.ratio;
            d.x *= root.scale_h;
            d.z *= root.scale_h;
            d.y *= root.scale_v;
            let tgt_rest = self.tgt_pose_mesh[root.tgt].to_scale_rotation_translation().2;
            root_mesh_pos = Some((root.tgt, tgt_rest + root.pose_offset + root.offset + d * root.translation_alpha));
        }

        // walk the target in hierarchy order, rebuilding local transforms
        let mut bones = vec![BoneTransform::IDENTITY; n];
        let mut mesh = vec![Mat4::IDENTITY; n];
        for &i in &target.order {
            let parent = target.parents[i];
            let (parent_mat, parent_rot) = match parent {
                Some(p) => (mesh[p], mesh[p].to_scale_rotation_translation().1),
                None => (Mat4::IDENTITY, Quat::IDENTITY),
            };
            let mut local = self.tgt_pose[i];
            if let Some(q) = want_mesh_rot[i] {
                local.rotation = (parent_rot.inverse() * q).normalize();
            }
            if let Some(t) = want_translation[i] {
                local.translation = t;
            }
            if let Some((root, pos)) = root_mesh_pos {
                if root == i {
                    local.translation = parent_mat.inverse().transform_point3(pos);
                }
            }
            bones[i] = local;
            mesh[i] = parent_mat * local.to_mat4();
        }

        let mut root_motion = src.root_motion;
        root_motion.translation *= self.ratio;
        if let Some(root) = &self.root {
            root_motion = RootMotion::IDENTITY.blend(&root_motion, root.rotation_alpha.max(root.translation_alpha));
        }
        let mut out = Pose { bones, curves: src.curves.clone(), root_motion, kind: src.kind };
        self.apply_ik(src, &src_mesh, &mut out);
        out
    }

    fn apply_ik(&self, src: &Pose, src_mesh: &[Mat4], out: &mut Pose) {
        let (Some(rig), Some(source_rig)) = (&self.target_rig, &self.source_rig) else { return };
        if self.ik.is_empty() {
            return;
        }
        let target = &*self.target;
        let tgt_mesh = target.to_mesh_space(&out.bones);
        let (src_root_pos, tgt_root_pos) = match &self.root {
            Some(r) => (src_mesh[r.src].to_scale_rotation_translation().2, tgt_mesh[r.tgt].to_scale_rotation_translation().2),
            None => (Vec3::ZERO, Vec3::ZERO),
        };
        let mut goals: GoalSet = vec![None; rig.goals.len()];
        for c in &self.ik {
            let chain = &rig.chains[c.chain];
            let Some(goal) = chain.goal else { continue };
            let end_src = *source_rig.chains[c.source_chain].bones.last().expect("resolved chain");
            let src_end = src_mesh[end_src].to_scale_rotation_translation().2;
            let mut rel = (src_end - src_root_pos) * self.ratio;
            rel.y *= c.scale_vertical;
            let mut goal_pos = tgt_root_pos + rel + c.static_offset;
            let start = chain.bones[0];
            let start_pos = tgt_mesh[start].to_scale_rotation_translation().2;
            goal_pos = start_pos + (goal_pos - start_pos) * c.extension;
            let (_, fk_rot, fk_pos) = tgt_mesh[*chain.bones.last().unwrap()].to_scale_rotation_translation();
            goals[goal] = Some(Effector { translation: fk_pos.lerp(goal_pos, c.blend), rotation: fk_rot });
        }
        let _ = src;
        rig.execute(target, &mut out.bones, &goals, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ik_retarget::ChainRetarget;
    use crate::ik_rig::{IkChain, IkGoal, IkRigAsset, IkSolver, SolverKind};
    use crate::math::{Transform, Vec3 as AV3};
    use crate::retarget::{BoneMapping, RootPolicy};
    use crate::skeleton::{Bone, Skeleton};

    /// root(0) -> spine(+h) -> arm(+h) with a custom segment length per skeleton.
    fn rig_skeleton(prefix: &str, h: f32, extra_mid: bool) -> RuntimeSkeleton {
        let mut bones = vec![
            ("root", None, [0.0, 0.0, 0.0]),
            ("spine", Some("root"), [0.0, h, 0.0]),
        ];
        if extra_mid {
            bones.push(("mid", Some("spine"), [0.0, h, 0.0]));
            bones.push(("arm", Some("mid"), [0.0, h, 0.0]));
        } else {
            bones.push(("arm", Some("spine"), [0.0, h * 2.0, 0.0]));
        }
        let sk = Skeleton {
            bones: bones
                .into_iter()
                .map(|(n, p, t)| Bone {
                    id: format!("{prefix}{n}"),
                    name: n.into(),
                    parent: p.map(|p| format!("{prefix}{p}")),
                    bind_transform: Transform { translation: AV3::new(t[0], t[1], t[2]), ..Transform::IDENTITY },
                })
                .collect(),
        };
        RuntimeSkeleton::new(prefix, &sk).unwrap()
    }

    fn profile() -> RetargetProfileAsset {
        let mut a = RetargetProfileAsset::new("p", None, None);
        for (s, t) in [("sroot", "troot"), ("sspine", "tspine"), ("sarm", "tarm")] {
            a.bone_map.push(BoneMapping { id: format!("m_{t}"), source_bone: s.into(), target_bone: t.into(), rotation: RotationMode::OneToOne, translation: TranslationMode::None });
        }
        a.root = RootPolicy { source_root: Some("sroot".into()), target_root: Some("troot".into()), ..RootPolicy::default() };
        a
    }

    #[test]
    fn rotation_deltas_transfer_between_different_proportions_and_root_scales() {
        let src = Arc::new(rig_skeleton("s", 1.0, false));
        let tgt = Arc::new(rig_skeleton("t", 2.0, false));
        let plan = RetargetPlan::from_profile(&profile(), src.clone(), tgt.clone(), None);
        assert!((plan.scale_ratio() - 2.0).abs() < 1e-5);
        assert!(plan.diagnostics.is_empty(), "{:?}", plan.diagnostics);
        // source spine bends 30 degrees about Z; root moves 1 unit in x
        let mut pose = src.bind_pose();
        pose.bones[1].rotation = Quat::from_rotation_z(0.5);
        pose.bones[0].translation = Vec3::new(1.0, 0.0, 0.0);
        pose.root_motion.translation = Vec3::new(0.0, 0.0, 1.0);
        pose.curves.insert("speed".into(), 3.0);
        let out = plan.retarget(&pose);
        assert!(out.bones[1].rotation.dot(Quat::from_rotation_z(0.5)) > 0.9999);
        // root translation is scaled by the size ratio
        assert!((out.bones[0].translation - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-4, "{:?}", out.bones[0].translation);
        // the target keeps its own bone lengths
        assert!((out.bones[1].translation - Vec3::new(0.0, 2.0, 0.0)).length() < 1e-5);
        assert!((out.root_motion.translation.z - 2.0).abs() < 1e-5);
        assert_eq!(out.curves["speed"], 3.0);
        // the bind pose retargets to the bind pose
        let rest = plan.retarget(&src.bind_pose());
        for (a, b) in rest.bones.iter().zip(&tgt.bind) {
            assert!((a.translation - b.translation).length() < 1e-5 && a.rotation.dot(b.rotation).abs() > 0.9999);
        }
    }

    #[test]
    fn retarget_pose_offsets_define_the_neutral_pose() {
        let src = Arc::new(rig_skeleton("s", 1.0, false));
        let tgt = Arc::new(rig_skeleton("t", 1.0, false));
        let mut a = profile();
        // the target's retarget pose has the spine tilted; the source's bind pose is straight
        a.target_pose.bone_rotations.push(crate::retarget::BoneRotation { bone: "tspine".into(), rotation: Quat::from_rotation_z(0.4).to_array() });
        let plan = RetargetPlan::from_profile(&a, src.clone(), tgt, None);
        let out = plan.retarget(&src.bind_pose());
        assert!(out.bones[1].rotation.dot(Quat::from_rotation_z(0.4)) > 0.9999, "the source's rest maps to the target's retarget pose");
    }

    #[test]
    fn interpolated_chains_distribute_rotation_across_different_bone_counts() {
        let src = Arc::new(rig_skeleton("s", 1.0, false)); // root, spine, arm
        let tgt = Arc::new(rig_skeleton("t", 1.0, true)); // root, spine, mid, arm
        let mut sa = IkRigAsset::new("s", None);
        sa.chains.push(IkChain { id: "c".into(), name: "arm".into(), start_bone: "sspine".into(), end_bone: "sarm".into(), goal: None });
        let mut ta = IkRigAsset::new("t", None);
        ta.chains.push(IkChain { id: "c".into(), name: "arm".into(), start_bone: "tspine".into(), end_bone: "tarm".into(), goal: None });
        let (srig, _) = CompiledIkRig::compile(&sa, &src);
        let (trig, _) = CompiledIkRig::compile(&ta, &tgt);
        let mut p = RetargetProfileAsset::new("p", None, None);
        p.source_rig = Some(pulsar_reflection::AssetRef::new());
        p.target_rig = Some(pulsar_reflection::AssetRef::new());
        p.chain_map.push(crate::retarget::ChainMapping {
            id: "m".into(), source_chain: "arm".into(), target_chain: "arm".into(), rotation: RotationMode::Interpolated, rotation_alpha: 1.0, translation: TranslationMode::None, translation_alpha: 1.0,
        });
        let plan = RetargetPlan::from_profile(&p, src.clone(), tgt.clone(), Some((Arc::new(srig), Arc::new(trig))));
        assert!(plan.diagnostics.iter().all(|d| d.code == "retarget.no_root"), "{:?}", plan.diagnostics);
        let mut pose = src.bind_pose();
        pose.bones[2].rotation = Quat::from_rotation_z(0.6);
        let out = plan.retarget(&pose);
        // the first target bone matches the first source bone (no rotation), the last matches the last
        assert!(out.bones[1].rotation.angle_between(Quat::IDENTITY) < 1e-4);
        let mesh = tgt.to_mesh_space(&out.bones);
        let tip_rot = mesh[3].to_scale_rotation_translation().1;
        assert!(tip_rot.dot(Quat::from_rotation_z(0.6)) > 0.999, "{tip_rot:?}");
        // the middle bone takes an in-between share
        let mid = mesh[2].to_scale_rotation_translation().1.angle_between(Quat::IDENTITY);
        assert!(mid > 0.05 && mid < 0.6, "{mid}");
        let again = plan.retarget(&pose);
        assert_eq!(out, again);
    }

    #[test]
    fn ik_retargeter_places_the_target_end_effector_from_the_source() {
        let src = Arc::new(rig_skeleton("s", 1.0, false));
        let tgt = Arc::new(rig_skeleton("t", 1.0, false));
        let build = |sk: &RuntimeSkeleton, p: &str| {
            let mut a = IkRigAsset::new(p, None);
            a.retarget_root = Some(format!("{p}root"));
            a.chains.push(IkChain { id: "c".into(), name: "arm".into(), start_bone: format!("{p}root"), end_bone: format!("{p}arm"), goal: Some("g".into()) });
            a.goals.push(IkGoal { id: "g".into(), name: "hand".into(), bone: format!("{p}arm"), position_alpha: 1.0, rotation_alpha: 1.0, default_offset: [0.0; 3] });
            a.solvers.push(IkSolver { id: "s".into(), name: "s".into(), enabled: true, kind: SolverKind::Fabrik { chain: "c".into(), goal: "g".into(), precision: 1e-4, max_iterations: 40, respect_joint_limits: false, take_goal_rotation: false } });
            Arc::new(CompiledIkRig::compile(&a, sk).0)
        };
        let (srig, trig) = (build(&src, "s"), build(&tgt, "t"));
        let mut asset = IkRetargetAsset::new("r", None, None);
        asset.chains.push(ChainRetarget {
            id: "c".into(), target_chain: "arm".into(), source_chain: Some("arm".into()),
            fk: crate::ik_retarget::FkSettings { enabled: false, ..Default::default() },
            ik: crate::ik_retarget::IkSettings { enabled: true, ..Default::default() },
        });
        let plan = RetargetPlan::from_ik_retargeter(&asset, src.clone(), tgt.clone(), srig, trig);
        assert!(plan.diagnostics.is_empty(), "{:?}", plan.diagnostics);
        let mut pose = src.bind_pose();
        pose.bones[1].rotation = Quat::from_rotation_z(-0.8);
        let src_end = src.to_mesh_space(&pose.bones)[2].to_scale_rotation_translation().2;
        let out = plan.retarget(&pose);
        let end = tgt.to_mesh_space(&out.bones)[2].to_scale_rotation_translation().2;
        assert!((end - src_end).length() < 2e-2, "{end} vs {src_end}");
    }
}
